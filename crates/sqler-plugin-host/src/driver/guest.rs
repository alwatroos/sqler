// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A guest driver (`kind = "guest"`): the WIT `driver` export, plus the optional `driver-catalog` export, seen as a
//! `Driver`, `Session`, `QueryStream` and `Introspect`.
//!
//! Every call goes through `PluginHost::run(.., CallKind::DriverCall, ..)`, so the sandbox, the deadline, the memory
//! cap and the three-faults rule apply as for any other call. A session is a resource inside one instance of the
//! plugin. When that instance is gone (a trap, a cancel in flight, a restart, a permission change) the session is
//! gone with it, and every later call fails with a fixed text instead of running against a stranger's handle.
//!
//! Cancel is the cancellation of the call in flight. The host drops the instance of a cancelled call, so the session
//! ends too; a cancel between two batches only ends the stream. The alternative, a guest `cancel` export, could never
//! run: the calls of one plugin are serialised and `next-batch` holds the plugin.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use sqler_driver_api::sqler_core::{
    ConnectionConfig, DbObject, Listing, ObjectKind, ObjectPath, Query, Table,
};
use sqler_driver_api::{
    BoxFuture, CancelHandle, Canceller, Driver, DriverError, Introspect, QueryEvent, QueryStream,
    Session,
};
use sqler_plugin_api::CapabilityKind;
use tokio::sync::Notify;
use wasmtime::component::ResourceAny;

use super::convert::{
    BATCH_ROWS, column_from_wit, column_info_from_wit, kind_from_wit, object_from_wit,
    rows_from_wit,
};
use super::error::{self, Phase};
use super::params::ConnectParams;
use super::source::Gate;
use super::{wit_catalog, wit_driver};
use crate::error::{CallError, GuestError};
use crate::query::{MAX_CATALOG_ITEMS, MAX_RESULT_BYTES, kind_to_wit, path_to_wit, value_to_wit};
use crate::runtime::wit::types as wit;
use crate::runtime::{CallFn, Live, PendingCall, PluginHost};
use crate::status::CallKind;

// ---------------------------------------------------------------------------------------------------------------
// The link between the adapter and the plugin
// ---------------------------------------------------------------------------------------------------------------

/// Which instance a handle belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ticket {
    generation: u64,
    instance: u64,
}

/// Why a call that reached the guest's instance did not produce its value.
enum Stop {
    /// The session's instance is gone, or the permission is no longer effective.
    Ended,
    /// `driver.guest` is not effective (at connect, where there is no session yet to end).
    Denied,
    /// The guest returned a `plugin-error`.
    Guest(GuestError),
}

type Step<T> = Result<T, Stop>;

/// A cancel request: a flag, and a wake-up for the call that waits.
#[derive(Default)]
struct Cancel {
    set: AtomicBool,
    wake: Notify,
}

impl Cancel {
    fn is_set(&self) -> bool {
        self.set.load(Ordering::SeqCst)
    }

    fn reset(&self) {
        self.set.store(false, Ordering::SeqCst);
    }
}

impl Canceller for Cancel {
    fn cancel(&self) -> BoxFuture<'_, Result<(), DriverError>> {
        self.set.store(true, Ordering::SeqCst);
        // `notify_one` keeps a permit when nobody waits, so a cancel just before the wait is not lost.
        self.wake.notify_one();
        Box::pin(std::future::ready(Ok(())))
    }
}

/// What a session and its streams share.
struct Link {
    host: PluginHost,
    gate: Gate,
    cancel: Arc<Cancel>,
    /// False once the instance this link lives in is known to be gone.
    alive: AtomicBool,
    ticket: OnceLock<Ticket>,
    /// Forms of the connect password a guest message must not echo (raw and percent-encoded).
    secrets: Vec<String>,
}

impl Link {
    fn new(host: PluginHost, gate: Gate, secrets: Vec<String>) -> Self {
        Self {
            host,
            gate,
            cancel: Arc::new(Cancel::default()),
            alive: AtomicBool::new(true),
            ticket: OnceLock::new(),
            secrets,
        }
    }

    fn ticket(&self) -> Ticket {
        self.ticket.get().copied().unwrap_or(Ticket {
            generation: u64::MAX,
            instance: 0,
        })
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    fn ended(&self) -> DriverError {
        self.alive.store(false, Ordering::SeqCst);
        DriverError::Unavailable(error::SESSION_ENDED.to_owned())
    }

    /// Runs `f` as a driver call. Waits for the result, or for a cancel request.
    async fn call<T: Send + 'static>(
        &self,
        phase: Phase,
        f: CallFn<Step<T>>,
    ) -> Result<T, DriverError> {
        if !self.is_alive() {
            return Err(self.ended());
        }
        let pending = self.host.run(&self.gate.plugin, CallKind::DriverCall, f);
        match self.wait(pending).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(Stop::Ended)) => Err(self.ended()),
            Ok(Err(Stop::Denied)) => Err(DriverError::Unavailable(error::not_approved(
                &self.gate.driver,
                self.gate.plugin.as_str(),
                "driver.guest",
            ))),
            Ok(Err(Stop::Guest(g))) => Err(error::guest(&g, phase, &self.secrets)),
            Err(e) => {
                if error::kills_instance(&e) {
                    self.alive.store(false, Ordering::SeqCst);
                }
                Err(error::from_call(&e, phase, &self.secrets))
            }
        }
    }

    /// Awaits `pending`; a cancel request cancels the call (which drops the instance) and ends the wait.
    async fn wait<R>(&self, mut pending: PendingCall<R>) -> Result<R, CallError> {
        loop {
            tokio::select! {
                biased;
                r = &mut pending => return r,
                () = self.cancel.wake.notified() => {
                    if self.cancel.is_set() {
                        pending.cancel();
                        return pending.await;
                    }
                }
            }
        }
    }

    /// Frees a guest resource without waiting: the call is spawned and its result dropped. Does nothing when the
    /// instance is known to be gone.
    fn release(&self, release: Release) {
        if !self.is_alive() {
            return;
        }
        let ticket = self.ticket();
        let f: CallFn<Step<()>> =
            Box::new(move |live: &mut Live| Box::pin(release_in(live, ticket, release)));
        drop(self.host.run(&self.gate.plugin, CallKind::DriverCall, f));
    }
}

/// A resource to free.
#[derive(Clone, Copy)]
enum Release {
    /// A row stream.
    Stream(ResourceAny),
    /// A session: closed, then dropped.
    Session(ResourceAny),
}

// ---------------------------------------------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------------------------------------------

/// The driver the registry gets for a guest plugin driver.
pub(super) struct GuestDriver {
    gate: Gate,
    host: PluginHost,
}

impl GuestDriver {
    pub(super) fn new(gate: Gate, host: PluginHost) -> Self {
        Self { gate, host }
    }
}

impl Driver for GuestDriver {
    fn id(&self) -> &str {
        &self.gate.driver
    }

    fn connect<'a>(
        &'a self,
        config: &'a ConnectionConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DriverError>> {
        Box::pin(async move {
            self.gate.check()?;
            // Only the parts of the target; nothing else of the connection config.
            let params = ConnectParams::parse(&config.target);
            let link = Arc::new(Link::new(
                self.host.clone(),
                self.gate.clone(),
                error::secrets(&params),
            ));
            let driver = self.gate.driver.clone();
            let f: CallFn<Step<Opened>> =
                Box::new(move |live: &mut Live| Box::pin(connect_in(live, driver, params)));
            let opened = link.call(Phase::Connect, f).await?;
            let _ = link.ticket.set(opened.ticket);
            let supported = opened
                .kinds
                .map(|kinds| kinds.into_iter().map(kind_from_wit).collect());
            Ok(Box::new(GuestSession {
                link,
                session: opened.session,
                supported,
                closed: false,
            }) as Box<dyn Session>)
        })
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The session
// ---------------------------------------------------------------------------------------------------------------

struct GuestSession {
    link: Arc<Link>,
    session: ResourceAny,
    /// The kinds `driver-catalog` lists, read at connect; `None` when the guest has no catalog export.
    supported: Option<Vec<ObjectKind>>,
    closed: bool,
}

impl Drop for GuestSession {
    fn drop(&mut self) {
        if !self.closed {
            self.link.release(Release::Session(self.session));
        }
    }
}

impl Session for GuestSession {
    fn cancel_handle(&self) -> CancelHandle {
        CancelHandle::new(Arc::clone(&self.link.cancel) as Arc<dyn Canceller>)
    }

    fn execute<'s>(
        &'s mut self,
        query: &'s Query,
    ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
        Box::pin(async move {
            self.link.cancel.reset();
            let params: Vec<wit::Value> = query.params.iter().map(value_to_wit).collect();
            let sql = query.sql.clone();
            let (ticket, session) = (self.link.ticket(), self.session);
            let f: CallFn<Step<Executed>> = Box::new(move |live: &mut Live| {
                Box::pin(execute_in(live, ticket, session, sql, params))
            });
            let executed = self.link.call(Phase::Run, f).await?;
            let mut stream = GuestStream {
                link: Arc::clone(&self.link),
                ticket,
                rows: None,
                width: 0,
                queue: VecDeque::new(),
                finished: false,
            };
            match executed {
                Executed::Affected(n) => {
                    stream.queue.push_back(QueryEvent::Columns(Vec::new()));
                    stream.queue.push_back(QueryEvent::Done {
                        rows_affected: Some(n),
                    });
                }
                Executed::Rows {
                    stream: rows,
                    columns,
                } => {
                    stream.rows = Some(rows);
                    stream.width = columns.len();
                    stream.queue.push_back(QueryEvent::Columns(
                        columns.into_iter().map(column_from_wit).collect(),
                    ));
                }
            }
            Ok(Box::new(stream) as Box<dyn QueryStream + 's>)
        })
    }

    fn introspection(&mut self) -> Option<&mut dyn Introspect> {
        if self.supported.is_some() {
            Some(self)
        } else {
            None
        }
    }

    fn close(mut self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
        self.closed = true;
        let link = Arc::clone(&self.link);
        let (ticket, session) = (link.ticket(), self.session);
        Box::pin(async move {
            if !link.is_alive() {
                return Ok(());
            }
            let f: CallFn<Step<()>> = Box::new(move |live: &mut Live| {
                Box::pin(release_in(live, ticket, Release::Session(session)))
            });
            match link.call(Phase::Run, f).await {
                Ok(()) => Ok(()),
                // The instance is gone, and the session with it: nothing is left to close.
                Err(DriverError::Unavailable(t)) if t == error::SESSION_ENDED => Ok(()),
                Err(e) => Err(e),
            }
        })
    }
}

impl Introspect for GuestSession {
    fn supports(&self, kind: ObjectKind) -> bool {
        self.supported
            .as_ref()
            .is_some_and(|kinds| kinds.contains(&kind))
    }

    fn list<'a>(
        &'a mut self,
        parent: &'a ObjectPath,
        kind: ObjectKind,
    ) -> BoxFuture<'a, Result<Listing, DriverError>> {
        Box::pin(async move {
            let Some(wit_kind) = kind_to_wit(kind).filter(|_| self.supports(kind)) else {
                return Ok(Listing::Unsupported);
            };
            self.link.cancel.reset();
            let parent = path_to_wit(parent.clone());
            let (ticket, session) = (self.link.ticket(), self.session);
            let f: CallFn<Step<Vec<wit::DbObject>>> = Box::new(move |live: &mut Live| {
                Box::pin(catalog_list_in(live, ticket, session, parent, wit_kind))
            });
            let items = self.link.call(Phase::Run, f).await?;
            if items.len() > MAX_CATALOG_ITEMS {
                return Err(DriverError::Protocol(error::MALFORMED_CATALOG.to_owned()));
            }
            Ok(Listing::Objects(
                items.into_iter().map(object_from_wit).collect(),
            ))
        })
    }

    fn table<'a>(
        &'a mut self,
        table: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<Table>, DriverError>> {
        Box::pin(async move {
            let Some(object) = object_to_wit(table) else {
                return Ok(None);
            };
            self.link.cancel.reset();
            let (ticket, session) = (self.link.ticket(), self.session);
            let f: CallFn<Step<Option<Vec<wit_catalog::TableColumn>>>> =
                Box::new(move |live: &mut Live| {
                    Box::pin(catalog_columns_in(live, ticket, session, object))
                });
            let Some(columns) = self.link.call(Phase::Run, f).await? else {
                return Ok(None);
            };
            if columns.len() > MAX_CATALOG_ITEMS {
                return Err(DriverError::Protocol(error::MALFORMED_CATALOG.to_owned()));
            }
            Ok(Some(Table::new(table.clone()).with_columns(
                columns.into_iter().map(column_info_from_wit).collect(),
            )))
        })
    }

    fn definition<'a>(
        &'a mut self,
        object: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<String>, DriverError>> {
        Box::pin(async move {
            let Some(object) = object_to_wit(object) else {
                return Ok(None);
            };
            self.link.cancel.reset();
            let (ticket, session) = (self.link.ticket(), self.session);
            let f: CallFn<Step<Option<String>>> = Box::new(move |live: &mut Live| {
                Box::pin(catalog_definition_in(live, ticket, session, object))
            });
            let text = self.link.call(Phase::Run, f).await?;
            if text.as_ref().is_some_and(|t| t.len() > MAX_RESULT_BYTES) {
                return Err(DriverError::Protocol(error::MALFORMED_CATALOG.to_owned()));
            }
            Ok(text)
        })
    }
}

/// The WIT view of an object; `None` for a kind the WIT package does not know.
fn object_to_wit(o: &DbObject) -> Option<wit::DbObject> {
    Some(wit::DbObject {
        kind: kind_to_wit(o.kind)?,
        parent: path_to_wit(o.parent.clone()),
        name: o.name.clone(),
    })
}

// ---------------------------------------------------------------------------------------------------------------
// The stream
// ---------------------------------------------------------------------------------------------------------------

struct GuestStream {
    link: Arc<Link>,
    ticket: Ticket,
    /// The guest's row stream while it has more batches.
    rows: Option<ResourceAny>,
    /// The number of result columns, for the width check.
    width: usize,
    /// Events that need no call: the columns, and the end of an affected-rows result.
    queue: VecDeque<QueryEvent>,
    finished: bool,
}

impl Drop for GuestStream {
    fn drop(&mut self) {
        if let Some(rows) = self.rows.take() {
            self.link.release(Release::Stream(rows));
        }
    }
}

impl QueryStream for GuestStream {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
        Box::pin(self.next())
    }
}

impl GuestStream {
    async fn next(&mut self) -> Option<Result<QueryEvent, DriverError>> {
        if self.finished {
            return None;
        }
        if self.link.cancel.is_set() {
            // Between batches: the stream ends and the session stays usable.
            self.end();
            return Some(Err(DriverError::Cancelled));
        }
        if let Some(event) = self.queue.pop_front() {
            self.finished = matches!(event, QueryEvent::Done { .. });
            return Some(Ok(event));
        }
        let Some(rows) = self.rows else {
            self.finished = true;
            return None;
        };
        let ticket = self.ticket;
        let f: CallFn<Step<Option<Vec<Vec<wit::Value>>>>> =
            Box::new(move |live: &mut Live| Box::pin(batch_in(live, ticket, rows)));
        match self.link.call(Phase::Run, f).await {
            Ok(Some(batch)) => {
                if let Ok(rows) = rows_from_wit(batch, self.width) {
                    Some(Ok(QueryEvent::Rows(rows)))
                } else {
                    self.end();
                    Some(Err(DriverError::Protocol(error::MALFORMED.to_owned())))
                }
            }
            Ok(None) => {
                // The guest dropped the resource when it reported the end.
                self.rows = None;
                self.finished = true;
                Some(Ok(QueryEvent::Done {
                    rows_affected: None,
                }))
            }
            Err(e) => {
                // The failed call dropped the resource, or the instance with it.
                self.rows = None;
                self.finished = true;
                Some(Err(e))
            }
        }
    }

    /// Ends the stream and frees the guest's row stream.
    fn end(&mut self) {
        self.finished = true;
        if let Some(rows) = self.rows.take() {
            self.link.release(Release::Stream(rows));
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// What runs inside the plugin's instance
// ---------------------------------------------------------------------------------------------------------------

/// A fresh session.
struct Opened {
    ticket: Ticket,
    session: ResourceAny,
    kinds: Option<Vec<wit::ObjectKind>>,
}

/// The outcome of `execute`.
enum Executed {
    Affected(u64),
    Rows {
        stream: ResourceAny,
        columns: Vec<wit::Column>,
    },
}

fn missing(interface: &'static str) -> wasmtime::Error {
    wasmtime::Error::new(CallError::MissingExport { interface })
}

fn driver_exports(live: &mut Live) -> wasmtime::Result<wit_driver::Guest> {
    let indices = wit_driver::GuestIndices::new(&live.pre).map_err(|_| missing("driver"))?;
    indices.load(&mut live.store, &live.instance)
}

fn catalog_exports(live: &mut Live) -> wasmtime::Result<wit_catalog::Guest> {
    let indices =
        wit_catalog::GuestIndices::new(&live.pre).map_err(|_| missing("driver-catalog"))?;
    indices.load(&mut live.store, &live.instance)
}

/// The handle belongs to this instance, and `driver.guest` is effective in it.
fn valid(live: &Live, ticket: Ticket) -> Step<()> {
    let same = live.generation == ticket.generation && live.instance_id == ticket.instance;
    if same
        && live
            .store
            .data()
            .effective
            .allows(CapabilityKind::DriverGuest)
    {
        Ok(())
    } else {
        Err(Stop::Ended)
    }
}

async fn connect_in(
    live: &mut Live,
    driver: String,
    params: ConnectParams,
) -> wasmtime::Result<Step<Opened>> {
    if !live
        .store
        .data()
        .effective
        .allows(CapabilityKind::DriverGuest)
    {
        return Ok(Err(Stop::Denied));
    }
    let guest = driver_exports(live)?;
    let wit_params = wit_driver::ConnectParams {
        host: params.host,
        database: params.database,
        user: params.user,
        password: params.password,
        port: params.port,
        options: params.options,
    };
    let session = match guest
        .call_connect(&mut live.store, &driver, &wit_params)
        .await?
    {
        Ok(session) => session,
        Err(e) => return Ok(Err(Stop::Guest(e.into()))),
    };
    let kinds = match wit_catalog::GuestIndices::new(&live.pre) {
        Ok(indices) => {
            let catalog = indices.load(&mut live.store, &live.instance)?;
            Some(
                catalog
                    .call_supported_kinds(&mut live.store, session)
                    .await?,
            )
        }
        Err(_) => None,
    };
    Ok(Ok(Opened {
        ticket: Ticket {
            generation: live.generation,
            instance: live.instance_id,
        },
        session,
        kinds,
    }))
}

async fn execute_in(
    live: &mut Live,
    ticket: Ticket,
    session: ResourceAny,
    sql: String,
    params: Vec<wit::Value>,
) -> wasmtime::Result<Step<Executed>> {
    if let Err(stop) = valid(live, ticket) {
        return Ok(Err(stop));
    }
    let guest = driver_exports(live)?;
    let execution = match guest
        .session()
        .call_execute(&mut live.store, session, &sql, &params)
        .await?
    {
        Ok(execution) => execution,
        Err(e) => return Ok(Err(Stop::Guest(e.into()))),
    };
    match execution {
        wit_driver::Execution::Affected(n) => Ok(Ok(Executed::Affected(n))),
        wit_driver::Execution::Rows(stream) => {
            let columns = guest
                .row_stream()
                .call_columns(&mut live.store, stream)
                .await?;
            Ok(Ok(Executed::Rows { stream, columns }))
        }
    }
}

/// The next batch. The row stream is dropped here when the guest reports the end or an error.
async fn batch_in(
    live: &mut Live,
    ticket: Ticket,
    rows: ResourceAny,
) -> wasmtime::Result<Step<Option<Vec<Vec<wit::Value>>>>> {
    if let Err(stop) = valid(live, ticket) {
        return Ok(Err(stop));
    }
    let guest = driver_exports(live)?;
    let next = guest
        .row_stream()
        .call_next_batch(&mut live.store, rows, BATCH_ROWS)
        .await?;
    match next {
        Ok(Some(batch)) => Ok(Ok(Some(batch))),
        Ok(None) => {
            rows.resource_drop_async(&mut live.store).await?;
            Ok(Ok(None))
        }
        Err(e) => {
            let _ = rows.resource_drop_async(&mut live.store).await;
            Ok(Err(Stop::Guest(e.into())))
        }
    }
}

async fn release_in(
    live: &mut Live,
    ticket: Ticket,
    release: Release,
) -> wasmtime::Result<Step<()>> {
    if let Err(stop) = valid(live, ticket) {
        return Ok(Err(stop));
    }
    match release {
        Release::Stream(rows) => rows.resource_drop_async(&mut live.store).await?,
        Release::Session(session) => {
            let guest = driver_exports(live)?;
            guest.session().call_close(&mut live.store, session).await?;
            session.resource_drop_async(&mut live.store).await?;
        }
    }
    Ok(Ok(()))
}

async fn catalog_list_in(
    live: &mut Live,
    ticket: Ticket,
    session: ResourceAny,
    parent: wit::ObjectPath,
    kind: wit::ObjectKind,
) -> wasmtime::Result<Step<Vec<wit::DbObject>>> {
    if let Err(stop) = valid(live, ticket) {
        return Ok(Err(stop));
    }
    let catalog = catalog_exports(live)?;
    Ok(
        match catalog
            .call_list(&mut live.store, session, &parent, kind)
            .await?
        {
            Ok(items) => Ok(items),
            Err(e) => Err(Stop::Guest(e.into())),
        },
    )
}

async fn catalog_columns_in(
    live: &mut Live,
    ticket: Ticket,
    session: ResourceAny,
    table: wit::DbObject,
) -> wasmtime::Result<Step<Option<Vec<wit_catalog::TableColumn>>>> {
    if let Err(stop) = valid(live, ticket) {
        return Ok(Err(stop));
    }
    let catalog = catalog_exports(live)?;
    Ok(
        match catalog
            .call_columns(&mut live.store, session, &table)
            .await?
        {
            Ok(columns) => Ok(columns),
            Err(e) => Err(Stop::Guest(e.into())),
        },
    )
}

async fn catalog_definition_in(
    live: &mut Live,
    ticket: Ticket,
    session: ResourceAny,
    object: wit::DbObject,
) -> wasmtime::Result<Step<Option<String>>> {
    if let Err(stop) = valid(live, ticket) {
        return Ok(Err(stop));
    }
    let catalog = catalog_exports(live)?;
    Ok(
        match catalog
            .call_definition(&mut live.store, session, &object)
            .await?
        {
            Ok(text) => Ok(text),
            Err(e) => Err(Stop::Guest(e.into())),
        },
    )
}

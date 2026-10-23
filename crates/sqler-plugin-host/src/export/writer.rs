// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The writer of a plugin export format: the WIT `exporter` export seen as a [`RowWriter`].
//!
//! Every call goes through `PluginHost::run(.., CallKind::ExportBatch, ..)` and blocks the export's own thread on
//! the answer. The guest returns bytes; this side writes them to the sink, so a plugin never sees a file, a path or
//! the file system. The writer resource lives in one instance of the plugin. When that instance is gone (a trap, a
//! cancel in flight, a restart, a permission change) every later call fails with a fixed text instead of running
//! against a stranger's handle.

use std::io::Write as _;

use sqler_driver_api::sqler_core::{Column, Row};
use sqler_export::{ExportError, RowWriter, Sink};
use sqler_plugin_api::PluginId;
use wasmtime::component::ResourceAny;

use super::convert::{columns_to_wit, rows_to_wit};
use super::error;
use crate::error::{CallError, GuestError};
use crate::runtime::wit::types as wit;
use crate::runtime::wit_exports::exporter as wit_exporter;
use crate::runtime::{CallFn, Live, PluginHost};
use crate::status::CallKind;

/// Which instance a resource belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ticket {
    generation: u64,
    instance: u64,
}

/// Why a call that reached the guest's instance did not produce its value.
enum Stop {
    /// The instance the writer lives in is gone.
    Ended,
    /// The guest returned a `plugin-error`.
    Guest(GuestError),
}

type Step<T> = Result<T, Stop>;

/// An open guest writer.
#[derive(Clone, Copy)]
struct Open {
    ticket: Ticket,
    resource: ResourceAny,
}

/// What the call in `begin` returns.
struct Started {
    ticket: Ticket,
    resource: ResourceAny,
}

/// The [`RowWriter`] of a plugin format.
pub(crate) struct PluginWriter<'a> {
    host: PluginHost,
    plugin: PluginId,
    /// The exporter id in the manifest, passed to `start`.
    exporter: String,
    /// The format label, for error texts.
    label: &'static str,
    sink: Sink<'a>,
    open: Option<Open>,
    /// True once the instance is known to be gone.
    dead: bool,
}

impl<'a> PluginWriter<'a> {
    pub(crate) fn new(
        host: PluginHost,
        plugin: PluginId,
        exporter: String,
        label: &'static str,
        sink: Sink<'a>,
    ) -> Self {
        Self {
            host,
            plugin,
            exporter,
            label,
            sink,
            open: None,
            dead: false,
        }
    }

    /// Runs `f` as an export call and waits for it on this thread.
    fn call<T: Send + 'static>(&mut self, f: CallFn<Step<T>>) -> Result<T, ExportError> {
        if self.dead {
            return Err(self.stopped());
        }
        let pending = self.host.run(&self.plugin, CallKind::ExportBatch, f);
        match self.host.wait_blocking(pending) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(Stop::Ended)) => {
                self.dead = true;
                Err(self.stopped())
            }
            Ok(Err(Stop::Guest(g))) => Err(error::guest(&g, self.label, self.plugin.as_str())),
            Err(e) => {
                if error::kills_instance(&e) {
                    self.dead = true;
                }
                Err(error::from_call(&e, self.label, self.plugin.as_str()))
            }
        }
    }

    fn stopped(&self) -> ExportError {
        ExportError::Plugin(error::stopped(self.label, self.plugin.as_str()))
    }

    fn open(&self) -> Result<Open, ExportError> {
        self.open
            .ok_or(ExportError::State("write_batch was called before begin"))
    }
}

impl RowWriter for PluginWriter<'_> {
    fn begin(&mut self, columns: &[Column]) -> Result<(), ExportError> {
        let (exporter, wit_columns) = (self.exporter.clone(), columns_to_wit(columns));
        let f: CallFn<Step<Started>> =
            Box::new(move |live: &mut Live| Box::pin(start_in(live, exporter, wit_columns)));
        let started = self.call(f)?;
        self.open = Some(Open {
            ticket: started.ticket,
            resource: started.resource,
        });
        Ok(())
    }

    fn write_batch(&mut self, rows: &[Row]) -> Result<(), ExportError> {
        let open = self.open()?;
        let wit_rows = rows_to_wit(rows);
        let f: CallFn<Step<Vec<u8>>> =
            Box::new(move |live: &mut Live| Box::pin(push_in(live, open, wit_rows)));
        let bytes = self.call(f)?;
        self.sink.write_all(&bytes)?;
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<(), ExportError> {
        let open = self.open()?;
        let f: CallFn<Step<Vec<u8>>> =
            Box::new(move |live: &mut Live| Box::pin(finish_in(live, open)));
        // The resource is dropped inside the call, whatever its outcome: nothing is left to release.
        self.open = None;
        let bytes = self.call(f)?;
        self.sink.write_all(&bytes)?;
        self.sink.flush()?;
        Ok(())
    }
}

impl Drop for PluginWriter<'_> {
    /// A writer dropped without `finish` (a cancel, an error elsewhere) frees its guest resource without waiting.
    /// The call is spawned and its result ignored.
    fn drop(&mut self) {
        let Some(open) = self.open.take() else {
            return;
        };
        if self.dead {
            return;
        }
        let f: CallFn<Step<()>> = Box::new(move |live: &mut Live| Box::pin(release_in(live, open)));
        drop(self.host.run(&self.plugin, CallKind::ExportBatch, f));
    }
}

// ---------------------------------------------------------------------------------------------------------------
// What runs inside the plugin's instance
// ---------------------------------------------------------------------------------------------------------------

fn missing() -> wasmtime::Error {
    wasmtime::Error::new(CallError::MissingExport {
        interface: "exporter",
    })
}

fn exports(live: &mut Live) -> wasmtime::Result<wit_exporter::Guest> {
    let indices = wit_exporter::GuestIndices::new(&live.pre).map_err(|_| missing())?;
    indices.load(&mut live.store, &live.instance)
}

/// The resource belongs to this instance.
fn valid(live: &Live, ticket: Ticket) -> Step<()> {
    if live.generation == ticket.generation && live.instance_id == ticket.instance {
        Ok(())
    } else {
        Err(Stop::Ended)
    }
}

async fn start_in(
    live: &mut Live,
    exporter: String,
    columns: Vec<wit::Column>,
) -> wasmtime::Result<Step<Started>> {
    let guest = exports(live)?;
    match guest
        .call_start(&mut live.store, &exporter, &columns)
        .await?
    {
        Ok(resource) => Ok(Ok(Started {
            ticket: Ticket {
                generation: live.generation,
                instance: live.instance_id,
            },
            resource,
        })),
        Err(e) => Ok(Err(Stop::Guest(e.into()))),
    }
}

async fn push_in(
    live: &mut Live,
    open: Open,
    rows: Vec<Vec<wit::Value>>,
) -> wasmtime::Result<Step<Vec<u8>>> {
    if let Err(stop) = valid(live, open.ticket) {
        return Ok(Err(stop));
    }
    let guest = exports(live)?;
    Ok(
        match guest
            .writer()
            .call_push(&mut live.store, open.resource, &rows)
            .await?
        {
            Ok(bytes) => Ok(bytes),
            Err(e) => Err(Stop::Guest(e.into())),
        },
    )
}

/// The trailing bytes. The resource is dropped here whether the guest succeeds or fails.
async fn finish_in(live: &mut Live, open: Open) -> wasmtime::Result<Step<Vec<u8>>> {
    if let Err(stop) = valid(live, open.ticket) {
        return Ok(Err(stop));
    }
    let guest = exports(live)?;
    let done = guest
        .writer()
        .call_finish(&mut live.store, open.resource)
        .await?;
    open.resource.resource_drop_async(&mut live.store).await?;
    Ok(match done {
        Ok(bytes) => Ok(bytes),
        Err(e) => Err(Stop::Guest(e.into())),
    })
}

async fn release_in(live: &mut Live, open: Open) -> wasmtime::Result<Step<()>> {
    if let Err(stop) = valid(live, open.ticket) {
        return Ok(Err(stop));
    }
    open.resource.resource_drop_async(&mut live.store).await?;
    Ok(Ok(()))
}

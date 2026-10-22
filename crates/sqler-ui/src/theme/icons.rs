// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The Lucide icons the shell draws and the asset source that serves them.
//!
//! `gpui_kit_assets::Assets` ships the icons `gpui-component` needs itself, which fixes the blank
//! component icons. The 24 icons it lacks are embedded here with `icon_assets!` (selected set
//! only, about +20 KiB). [`AppAssets`] tries those first and falls back to `Assets`. Register it
//! with `Application::with_assets`. The icons are ISC licensed, see `licenses/LUCIDE-ISC.txt`.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};
use gpui_kit_assets::{Assets, IconName, IconNamed};

// Only the icons that `Assets` does not already serve.
gpui_kit_assets::icon_assets!(
    LucideIcons,
    [
        Database,
        Server,
        Table,
        Table2,
        KeyRound,
        SquareFunction,
        SquarePlay,
        CircleStop,
        ListTree,
        FileCode,
        SlidersHorizontal,
        Clock,
        Bookmark,
        X,
        Columns2,
        Pin,
        MessageSquare,
        WandSparkles,
        Sparkles,
        Save,
        Plug,
        Unplug,
        Command,
        Wrench,
    ]
);

/// An icon of the application's set; each is the `IconName` of the same name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AppIcon {
    /// The Lucide `database` icon.
    Database,
    /// The Lucide `server` icon.
    Server,
    /// The Lucide `table` icon.
    Table,
    /// The Lucide `table2` icon.
    Table2,
    /// The Lucide `eye` icon.
    Eye,
    /// The Lucide `key round` icon.
    KeyRound,
    /// The Lucide `square function` icon.
    SquareFunction,
    /// The Lucide `folder` icon.
    Folder,
    /// The Lucide `folder open` icon.
    FolderOpen,
    /// The Lucide `play` icon.
    Play,
    /// The Lucide `square play` icon.
    SquarePlay,
    /// The Lucide `circle stop` icon.
    CircleStop,
    /// The Lucide `list tree` icon.
    ListTree,
    /// The Lucide `file code` icon.
    FileCode,
    /// The Lucide `file text` icon.
    FileText,
    /// The Lucide `sliders horizontal` icon.
    SlidersHorizontal,
    /// The Lucide `clock` icon.
    Clock,
    /// The Lucide `bookmark` icon.
    Bookmark,
    /// The Lucide `settings` icon.
    Settings,
    /// The Lucide `search` icon.
    Search,
    /// The Lucide `x` icon.
    X,
    /// The Lucide `plus` icon.
    Plus,
    /// The Lucide `chevron right` icon.
    ChevronRight,
    /// The Lucide `chevron down` icon.
    ChevronDown,
    /// The Lucide `panel left` icon.
    PanelLeft,
    /// The Lucide `panel bottom` icon.
    PanelBottom,
    /// The Lucide `columns2` icon.
    Columns2,
    /// The Lucide `pin` icon.
    Pin,
    /// The Lucide `message square` icon.
    MessageSquare,
    /// The Lucide `wand sparkles` icon.
    WandSparkles,
    /// The Lucide `sparkles` icon.
    Sparkles,
    /// The Lucide `save` icon.
    Save,
    /// The Lucide `refresh cw` icon.
    RefreshCw,
    /// The Lucide `copy` icon.
    Copy,
    /// The Lucide `plug` icon.
    Plug,
    /// The Lucide `unplug` icon.
    Unplug,
    /// The Lucide `triangle alert` icon.
    TriangleAlert,
    /// The Lucide `circle alert` icon.
    CircleAlert,
    /// The Lucide `info` icon.
    Info,
    /// The Lucide `check` icon.
    Check,
    /// The Lucide `ellipsis` icon.
    Ellipsis,
    /// The Lucide `loader circle` icon.
    LoaderCircle,
    /// The Lucide `wrench` icon.
    Wrench,
    /// The Lucide `command` icon.
    Command,
    /// The Lucide `bot` icon (served by `gpui-component`'s own assets).
    Bot,
}

impl AppIcon {
    /// Every icon of the set.
    pub const ALL: [AppIcon; 45] = [
        Self::Database,
        Self::Server,
        Self::Table,
        Self::Table2,
        Self::Eye,
        Self::KeyRound,
        Self::SquareFunction,
        Self::Folder,
        Self::FolderOpen,
        Self::Play,
        Self::SquarePlay,
        Self::CircleStop,
        Self::ListTree,
        Self::FileCode,
        Self::FileText,
        Self::SlidersHorizontal,
        Self::Clock,
        Self::Bookmark,
        Self::Settings,
        Self::Search,
        Self::X,
        Self::Plus,
        Self::ChevronRight,
        Self::ChevronDown,
        Self::PanelLeft,
        Self::PanelBottom,
        Self::Columns2,
        Self::Pin,
        Self::MessageSquare,
        Self::WandSparkles,
        Self::Sparkles,
        Self::Save,
        Self::RefreshCw,
        Self::Copy,
        Self::Plug,
        Self::Unplug,
        Self::TriangleAlert,
        Self::CircleAlert,
        Self::Info,
        Self::Check,
        Self::Ellipsis,
        Self::LoaderCircle,
        Self::Command,
        Self::Wrench,
        Self::Bot,
    ];

    /// The `gpui-kit-assets` name of this icon.
    #[must_use]
    pub fn name(self) -> IconName {
        match self {
            Self::Database => IconName::Database,
            Self::Server => IconName::Server,
            Self::Table => IconName::Table,
            Self::Table2 => IconName::Table2,
            Self::Eye => IconName::Eye,
            Self::KeyRound => IconName::KeyRound,
            Self::SquareFunction => IconName::SquareFunction,
            Self::Folder => IconName::Folder,
            Self::FolderOpen => IconName::FolderOpen,
            Self::Play => IconName::Play,
            Self::SquarePlay => IconName::SquarePlay,
            Self::CircleStop => IconName::CircleStop,
            Self::ListTree => IconName::ListTree,
            Self::FileCode => IconName::FileCode,
            Self::FileText => IconName::FileText,
            Self::SlidersHorizontal => IconName::SlidersHorizontal,
            Self::Clock => IconName::Clock,
            Self::Bookmark => IconName::Bookmark,
            Self::Settings => IconName::Settings,
            Self::Search => IconName::Search,
            Self::X => IconName::X,
            Self::Plus => IconName::Plus,
            Self::ChevronRight => IconName::ChevronRight,
            Self::ChevronDown => IconName::ChevronDown,
            Self::PanelLeft => IconName::PanelLeft,
            Self::PanelBottom => IconName::PanelBottom,
            Self::Columns2 => IconName::Columns2,
            Self::Pin => IconName::Pin,
            Self::MessageSquare => IconName::MessageSquare,
            Self::WandSparkles => IconName::WandSparkles,
            Self::Sparkles => IconName::Sparkles,
            Self::Save => IconName::Save,
            Self::RefreshCw => IconName::RefreshCw,
            Self::Copy => IconName::Copy,
            Self::Plug => IconName::Plug,
            Self::Unplug => IconName::Unplug,
            Self::TriangleAlert => IconName::TriangleAlert,
            Self::CircleAlert => IconName::CircleAlert,
            Self::Info => IconName::Info,
            Self::Check => IconName::Check,
            Self::Ellipsis => IconName::Ellipsis,
            Self::LoaderCircle => IconName::LoaderCircle,
            Self::Command => IconName::Command,
            Self::Wrench => IconName::Wrench,
            Self::Bot => IconName::Bot,
        }
    }
}

impl IconNamed for AppIcon {
    fn path(self) -> SharedString {
        self.name().path()
    }
}

/// The application's asset source: the embedded Lucide extras, else `gpui_kit_assets::Assets`.
#[derive(Clone, Copy, Debug, Default)]
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = LucideIcons.load(path)? {
            return Ok(Some(bytes));
        }
        Assets.load(path)
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let mut paths = Assets.list(path)?;
        paths.extend(LucideIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_loads() {
        for icon in AppIcon::ALL {
            let path = icon.path();
            let bytes = AppAssets.load(&path).expect("load");
            assert!(bytes.is_some_and(|b| !b.is_empty()), "{path}");
        }
    }

    #[test]
    fn all_is_unique() {
        let mut v = AppIcon::ALL.to_vec();
        v.dedup();
        assert_eq!(v.len(), 45);
        let set: std::collections::HashSet<_> = AppIcon::ALL.into_iter().collect();
        assert_eq!(set.len(), 45);
    }

    #[test]
    fn extras_are_not_served_by_the_component_assets() {
        for path in LucideIcons.list("").expect("list") {
            // `Assets` errors on unknown paths, which is what "not served" means.
            assert!(Assets.load(&path).is_err(), "{path} is already in Assets");
        }
    }

    #[test]
    fn component_icons_still_load() {
        let bytes = AppAssets.load("icons/close.svg").expect("load");
        assert!(bytes.is_some());
    }

    #[test]
    fn unknown_path_is_an_error_or_none() {
        assert!(
            AppAssets
                .load("icons/nope.svg")
                .map_or(true, |b| b.is_none())
        );
    }
}

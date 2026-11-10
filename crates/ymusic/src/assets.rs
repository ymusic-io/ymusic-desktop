//! The default component assets plus the extra Lucide icons the shell uses.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

gpui_kit::assets::icon_assets!(
    ShellIcons,
    [
        MonitorPlay,
        CirclePlay,
        LibraryBig,
        RotateCcwClock,
        Download,
        Settings,
        Heart,
        Send,
        MessageCircle,
        Play,
        Pause,
        SkipBack,
        SkipForward,
        Volume2,
        Repeat,
        Repeat1,
        Shuffle,
        Timer,
        RotateCcw,
        FolderOpen,
        X,
        SlidersHorizontal,
        User,
        Disc,
        Music,
        Film,
        Folder,
        House,
        TrendingUp,
        Star,
        Menu,
        ArrowUpLeft,
        ArrowLeft,
        Search,
        RotateCw,
        Captions,
        Maximize,
        Minimize,
        ShieldCheck,
        ShieldOff,
        ListPlus,
        ListCheck,
        Share2,
        RefreshCw,
        ArrowUpDown,
        Tag
    ]
);

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match ShellIcons.load(path) {
            Ok(Some(bytes)) => Ok(Some(bytes)),
            _ => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names = ShellIcons.list(path).unwrap_or_default();
        names.extend(gpui_kit::assets::Assets.list(path)?);
        Ok(names)
    }
}

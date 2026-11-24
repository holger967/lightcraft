//! HDR display output (LR-VIEW-HDR-DISPLAY): whether the window can show the range above SDR
//! white, and with which surface format; and, while it can, the display's SDR white and headroom
//! ([`DisplayRange`]). The window falls back to SDR when any condition is missing, so an HDR edit
//! is never shown wrong.
//!
//! HDR needs three things: a platform path (macOS EDR via an extended-range float Metal layer,
//! Windows HDR via scRGB float or HDR10 swap chains, Wayland via the colour-management protocol),
//! a surface format that carries values above 1.0 (`Rgba16Float`) or a 10-bit PQ signal
//! (`Rgb10a2Unorm`), and a display with headroom above SDR white. The window asks for a float
//! scRGB / EDR surface with `LIGHTCRAFT_HDR_DISPLAY=1` ([`surface_requested`]); HDR edits are then
//! shown as gain map viewers show them, blended for the display's headroom
//! (`lightcraft_engine::media::render::adapt_hdr`).

use eframe::wgpu::TextureFormat;

/// Windowing platform, as far as HDR output is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Windows,
    Wayland,
    X11,
    Web,
    Other,
}

impl Platform {
    /// The platform this build runs on (`wayland` from the session environment on Linux / BSD).
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_arch = "wasm32") {
            Self::Web
        } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            Self::Wayland
        } else if std::env::var_os("DISPLAY").is_some() {
            Self::X11
        } else {
            Self::Other
        }
    }

    /// Whether the platform has an HDR compositing path wgpu can reach at all.
    fn has_hdr_path(self) -> bool {
        matches!(self, Self::MacOs | Self::Windows | Self::Wayland)
    }
}

/// How HDR values reach the display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HdrEncoding {
    /// Linear float, SDR white at 1.0 and the headroom above it (macOS EDR, Windows scRGB,
    /// Wayland extended linear).
    ExtendedLinear,
    /// 10-bit BT.2100 PQ (Windows HDR10, Wayland PQ).
    Pq10,
}

/// The display output decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayOutput {
    Sdr { reason: &'static str },
    Hdr { format: TextureFormat, encoding: HdrEncoding },
}

impl DisplayOutput {
    pub fn is_hdr(self) -> bool {
        matches!(self, Self::Hdr { .. })
    }
}

/// The encoding a surface format carries on `platform`, if it is an HDR one.
fn encoding_of(format: TextureFormat, platform: Platform) -> Option<HdrEncoding> {
    match format {
        TextureFormat::Rgba16Float => Some(HdrEncoding::ExtendedLinear),
        // macOS takes HDR through EDR floats only
        TextureFormat::Rgb10a2Unorm if platform != Platform::MacOs => Some(HdrEncoding::Pq10),
        _ => None,
    }
}

/// The best HDR surface format among those a surface offers (`available`), for `platform`:
/// float first (keeps SDR white at 1.0 and needs no PQ encode), then 10-bit PQ. `None` = SDR only.
/// (For configuring the surface once eframe lets the app pick its format.)
#[cfg_attr(not(test), allow(dead_code))]
pub fn preferred_hdr_format(available: &[TextureFormat], platform: Platform) -> Option<TextureFormat> {
    if !platform.has_hdr_path() {
        return None;
    }
    [TextureFormat::Rgba16Float, TextureFormat::Rgb10a2Unorm].into_iter().find(|f| available.contains(f) && encoding_of(*f, platform).is_some())
}

/// The decision for the window: `target` is the format the renderer configured (`None`: the glow
/// renderer or no wgpu state), `display_headroom` the display's current peak over SDR white as the
/// OS reports it (`None`: unknown, `Some(1.0)`: an SDR display or HDR switched off in the OS),
/// `disabled` the user's opt-out (`LIGHTCRAFT_HDR_DISPLAY=0`).
pub fn choose(target: Option<TextureFormat>, platform: Platform, display_headroom: Option<f32>, disabled: bool) -> DisplayOutput {
    if disabled {
        return DisplayOutput::Sdr { reason: "HDR display is switched off" };
    }
    if !platform.has_hdr_path() {
        return DisplayOutput::Sdr { reason: "this windowing system has no HDR output" };
    }
    let Some(format) = target else {
        return DisplayOutput::Sdr { reason: "the window renderer is not wgpu" };
    };
    let Some(encoding) = encoding_of(format, platform) else {
        return DisplayOutput::Sdr { reason: "the window surface is 8-bit" };
    };
    match display_headroom {
        Some(h) if h.is_finite() && h > 1.0 => DisplayOutput::Hdr { format, encoding },
        Some(_) => DisplayOutput::Sdr { reason: "the display is in SDR mode" },
        None => DisplayOutput::Sdr { reason: "the display's HDR headroom is not known yet (asked once the window shows)" },
    }
}

/// Whether to ask for an HDR window surface (`Rgba16Float` scRGB / EDR) at start: only with
/// `LIGHTCRAFT_HDR_DISPLAY=1` while the HDR view is being finished.
pub fn surface_requested() -> bool {
    std::env::var("LIGHTCRAFT_HDR_DISPLAY").ok().is_some_and(|v| matches!(v.trim(), "1" | "on" | "true" | "yes"))
}

/// What the UI's white is written as on an scRGB surface, where 1.0 is 80 nits: `sdr_white_nits / 80`.
/// `None` (keep 1.0) for a missing, non-finite or non-positive level.
pub fn sdr_white_scale(sdr_white_nits: Option<f32>) -> Option<f32> {
    sdr_white_nits.filter(|n| n.is_finite() && *n > 0.0).map(|n| n / SCRGB_NITS)
}

/// The luminance scRGB's 1.0 stands for.
const SCRGB_NITS: f32 = 80.0;

/// The SDR white level set by hand (`LIGHTCRAFT_SDR_WHITE_NITS`), which overrides the display's.
pub fn sdr_white_nits_from_env() -> Option<f32> {
    std::env::var("LIGHTCRAFT_SDR_WHITE_NITS").ok().and_then(|v| v.trim().parse().ok())
}

/// The display's headroom over `sdr_white_nits` (`None`: unknown): with a white set by hand,
/// measured against that white (the display's peak in nits over it), so the photo's SDR white
/// and its brightest highlight stay in step with the UI; otherwise as the display reports it.
pub fn headroom(info: &eframe::wgpu::DisplayHdrInfo, white_override: Option<f32>) -> Option<f32> {
    match (white_override.filter(|n| n.is_finite() && *n > 0.0), info.luminance.and_then(|l| l.max_nits)) {
        (Some(white), Some(max)) if max.is_finite() => Some(max / white),
        _ => info.tone_map_headroom(),
    }
}

/// Follows the display the window is on, on an scRGB / EDR surface: its SDR white (the UI's white
/// is written at it) and its HDR headroom (HDR edits are blended for it). egui-wgpu asks the OS
/// about once a second while frames are drawn (`RenderState::display_hdr`); a repaint a second
/// keeps that going while the app is idle, so a change in Windows' HDR settings or a move to
/// another monitor shows within a second.
pub struct DisplayRange {
    info: std::sync::Arc<egui::mutex::RwLock<Option<eframe::wgpu::DisplayHdrInfo>>>,
    renderer: std::sync::Arc<egui::mutex::RwLock<eframe::egui_wgpu::Renderer>>,
    target: TextureFormat,
    white_override: Option<f32>,
    white: Option<f32>,
    decision: Option<DisplayOutput>,
}

impl DisplayRange {
    /// For the window `cc` belongs to; `None` unless its surface is scRGB / EDR.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Option<Self> {
        let rs = cc.wgpu_render_state.as_ref().filter(|r| r.target_color_space == eframe::wgpu::SurfaceColorSpace::ExtendedSrgbLinear)?;
        Some(Self {
            info: rs.display_hdr.clone(),
            renderer: rs.renderer.clone(),
            target: rs.target_format,
            white_override: sdr_white_nits_from_env(),
            white: None,
            decision: None,
        })
    }

    /// Apply what the display reports now; the headroom HDR edits are shown with (`None`: SDR).
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<f32> {
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
        let info = self.info.read().clone().unwrap_or_default();
        let white = self.white_override.or(info.luminance.and_then(|l| l.sdr_white_nits));
        if white != self.white {
            self.white = white;
            // a monitor without a level (or none known yet) shows the UI at scRGB's own white
            let scale = sdr_white_scale(white);
            match (white, scale) {
                (Some(n), Some(_)) => log::info!("display: SDR white {n:.0} nits"),
                _ => log::info!("display: SDR white not known; the UI is drawn at 80 nits"),
            }
            self.renderer.write().set_sdr_white_scale(scale.unwrap_or(1.0));
        }
        let headroom = headroom(&info, self.white_override);
        let decision = choose(Some(self.target), Platform::current(), headroom, false);
        if self.decision != Some(decision) {
            self.decision = Some(decision);
            log_decision(decision, headroom);
        }
        headroom.filter(|_| decision.is_hdr())
    }
}

fn log_decision(out: DisplayOutput, headroom: Option<f32>) {
    match out {
        DisplayOutput::Sdr { reason } => log::info!("display: SDR ({reason})"),
        DisplayOutput::Hdr { format, encoding } => {
            log::info!("display: HDR {encoding:?} ({format:?}), {:.2} stops above SDR white", headroom.unwrap_or(1.0).log2());
        }
    }
}

/// The decision at start, logged (the window renderer's `target` format): until the window first
/// draws, the display's headroom isn't known, so an HDR-ready window says so here and
/// [`DisplayRange`] logs the decision once it is.
pub fn report(target: Option<TextureFormat>) -> DisplayOutput {
    let disabled = std::env::var("LIGHTCRAFT_HDR_DISPLAY").ok().is_some_and(|v| matches!(v.trim(), "0" | "off" | "false" | "no"));
    let out = choose(target, Platform::current(), None, disabled);
    log_decision(out, None);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use TextureFormat as F;

    #[test]
    fn eight_bit_surfaces_are_sdr() {
        for p in [Platform::MacOs, Platform::Windows, Platform::Wayland] {
            for f in [F::Bgra8Unorm, F::Bgra8UnormSrgb, F::Rgba8Unorm] {
                assert!(!choose(Some(f), p, Some(4.0), false).is_hdr(), "{p:?} {f:?}");
            }
        }
    }

    #[test]
    fn hdr_needs_platform_format_and_headroom() {
        let hdr = DisplayOutput::Hdr { format: F::Rgba16Float, encoding: HdrEncoding::ExtendedLinear };
        assert_eq!(choose(Some(F::Rgba16Float), Platform::MacOs, Some(2.0), false), hdr);
        assert_eq!(
            choose(Some(F::Rgb10a2Unorm), Platform::Windows, Some(4.0), false),
            DisplayOutput::Hdr { format: F::Rgb10a2Unorm, encoding: HdrEncoding::Pq10 }
        );
        // any missing condition falls back to SDR
        assert!(!choose(Some(F::Rgba16Float), Platform::X11, Some(4.0), false).is_hdr());
        assert!(!choose(Some(F::Rgba16Float), Platform::Web, Some(4.0), false).is_hdr());
        assert!(!choose(Some(F::Rgba16Float), Platform::MacOs, Some(1.0), false).is_hdr());
        assert!(!choose(Some(F::Rgba16Float), Platform::MacOs, None, false).is_hdr());
        assert!(!choose(Some(F::Rgba16Float), Platform::MacOs, Some(f32::NAN), false).is_hdr());
        assert!(!choose(Some(F::Rgba16Float), Platform::MacOs, Some(2.0), true).is_hdr());
        assert!(!choose(None, Platform::Windows, Some(4.0), false).is_hdr());
        // macOS has no PQ swap chain path
        assert!(!choose(Some(F::Rgb10a2Unorm), Platform::MacOs, Some(4.0), false).is_hdr());
    }

    #[test]
    fn sdr_white_scale_is_relative_to_scrgb_white() {
        let scale = |n| sdr_white_scale(Some(n));
        assert!(scale(240.0) > scale(80.0), "a brighter SDR white writes the UI brighter");
        assert_eq!(scale(80.0), Some(1.0), "80 nits is scRGB's own white");
        for broken in [0.0, -5.0, f32::NAN, f32::INFINITY] {
            assert_eq!(scale(broken), None, "{broken}");
        }
        assert_eq!(sdr_white_scale(None), None);
    }

    #[test]
    fn a_white_set_by_hand_measures_the_headroom_against_it() {
        use eframe::wgpu::{DisplayHdrInfo, DisplayLuminance};
        let info = DisplayHdrInfo {
            luminance: Some(DisplayLuminance { max_nits: Some(600.0), sdr_white_nits: Some(200.0), ..Default::default() }),
            ..Default::default()
        };
        let reported = headroom(&info, None).unwrap_or(0.0);
        assert!((reported - 3.0).abs() < 1e-3, "as the display reports it: {reported}");
        assert!(headroom(&info, Some(100.0)) > Some(reported), "a dimmer white set by hand leaves more room above it");
        assert_eq!(headroom(&DisplayHdrInfo::default(), Some(100.0)), None, "no peak known: no headroom");
    }

    #[test]
    fn preferred_format_picks_float_then_pq() {
        let all = [F::Bgra8Unorm, F::Rgb10a2Unorm, F::Rgba16Float];
        assert_eq!(preferred_hdr_format(&all, Platform::Windows), Some(F::Rgba16Float));
        assert_eq!(preferred_hdr_format(&[F::Bgra8Unorm, F::Rgb10a2Unorm], Platform::Wayland), Some(F::Rgb10a2Unorm));
        assert_eq!(preferred_hdr_format(&[F::Bgra8Unorm, F::Rgb10a2Unorm], Platform::MacOs), None);
        assert_eq!(preferred_hdr_format(&all, Platform::X11), None);
        assert_eq!(preferred_hdr_format(&[], Platform::MacOs), None);
    }
}

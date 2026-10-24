//! The popover's chrome: icon tab strip, scrolling content, footer.
//!
//! Views read the store directly and return elements, rather than being
//! gpui views of their own — there is one root view and the whole panel
//! repaints when a tick lands, which every 2–5s over 320px is not worth
//! splitting up.
//!
//! **Affordance rule.** macOS does not change the pointer over clickable
//! things: AppKit keeps the arrow over buttons, tabs and table rows alike,
//! and reserves `pointingHand` for links — WebKit uses it for hyperlinks and
//! NSTextView for `.link` runs, so on this platform the hand means "this
//! leaves where you are", not "this is clickable". Every clickable element
//! therefore needs a *visible* affordance: a hover fill, a border that lifts,
//! or real button chrome. Where the target is a whole card rather than
//! something button-shaped, hover is not enough and it gets an explicit
//! control instead — that is why the Alerts card carries a gear button rather
//! than being clickable along its title.
//!
//! `cursor_pointer` is consequently used exactly once, on the footer's GitHub
//! button — the only control here that opens something outside the app. Used
//! that sparingly it carries the platform's own meaning instead of reading as
//! a web page in a window.

mod alerts;
mod apps;
// Public: rendered by the settings window (main.rs), not by a tab.
pub mod config;
mod disk;
mod drives;
mod gpu;
mod history;
mod listening;
mod net;
mod overview;
mod processes;
mod sensors;
pub mod storage;
mod traffic;

pub mod widgets;

use crate::assets::CustomIconName;
use crate::awake;
use crate::font;
use crate::format;
use crate::gpui_ui;
use crate::i18n;
use crate::prefs;
use crate::state::{Tab, ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    Anchor, AnyElement, App, Div, ElementId, Hsla, InteractiveElement, Interactivity, IntoElement,
    ParentElement, RenderOnce, SharedString, Stateful, StatefulInteractiveElement, StyleRefinement,
    Styled, Window, div, px,
};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, IconName, Selectable, Sizable, Size, h_flex, v_flex};
use rust_i18n::t;
use std::time::SystemTime;

/// The panel's one spacing scale (`.claude/skills/ui-polish`): 4, 8, 10,
/// 12, 16 and nothing between. 12 is the gutter round everything and 10
/// the gap between the things inside it. Before 2026-10-12 the frame was
/// 14, the cards 12 apart with 13 of padding, and rows 5, 6 and 7 from
/// each other — every value chosen once, for one card.
const GUTTER: f32 = 12.;

/// Height of the tab strip: the gutter above, a 38-tall well (4 + 30 +
/// 4), and the gap to the first card under it.
///
/// Written down, and applied as an explicit height below, because a view
/// that has to size something against the viewport has no other way to know
/// how much room the body was left — `uniform_list` cannot measure itself,
/// it needs a bound to virtualise against.
const TAB_STRIP_HEIGHT: f32 = GUTTER + 38. + CARD_GAP;

/// Height of the footer: a 30-tall button row centred in 44, hairline
/// included.
const FOOTER_HEIGHT: f32 = 44.;

/// What the body keeps clear beneath its content — for every tab: the
/// same gap the cards keep from each other, above the footer's hairline.
///
/// Removing it entirely was tried and was wrong: the tabs whose last
/// card paints rows to its own edge (Processes, Apps, History) then sat
/// flush against that hairline.
const BODY_BOTTOM_PAD: f32 = CARD_GAP;

/// Space above the first card. None: the tab strip ends on the gap.
/// Kept as a name because lists that size against [`body_height`]
/// subtract it.
const CONTENT_TOP_PAD: f32 = 0.;

/// Space between cards, and between the tab strip and the first.
const CARD_GAP: f32 = 10.;

/// Room the scrolling body actually has, or `None` until the first frame
/// has reported a window size.
pub(super) fn body_height(state: &ZStatsAppState) -> Option<f32> {
    state.window_bounds().map(|bounds| {
        bounds.size.height - TAB_STRIP_HEIGHT - FOOTER_HEIGHT - BODY_BOTTOM_PAD - CONTENT_TOP_PAD
    })
}

/// Whether the short Overview is showing its notice row. The one fact
/// about a page the window needs: the panel is that row taller while it
/// is (`crate::sync_panel_notice`), so the question is asked here and
/// answered by the page itself.
pub fn overview_has_notice(state: &ZStatsAppState) -> bool {
    !prefs::overview_detail() && overview::has_notice(state)
}

/// Build the whole panel.
pub fn root(cx: &App) -> AnyElement {
    let state = cx.global::<ZStatsGlobalStore>().read(cx);
    v_flex()
        .size_full()
        .child(tab_strip(state))
        .child(content(state))
        .child(footer(state))
        .into_any_element()
}

fn tab_icon(tab: Tab) -> Icon {
    match tab {
        Tab::Overview => Icon::new(IconName::LayoutDashboard),
        Tab::Apps => CustomIconName::AppWindow.into(),
        Tab::Processes => Icon::new(IconName::Cpu),
        Tab::Hardware => Icon::new(IconName::HardDrive),
        Tab::Net => Icon::new(IconName::Network),
        Tab::Alerts => Icon::new(IconName::Bell),
        Tab::History => CustomIconName::History.into(),
    }
}

/// How the tab shortcuts are spelled for this platform. gpui's
/// `secondary-` modifier is ⌘ on macOS and Ctrl everywhere else (Super
/// belongs to the compositor on a Hyprland desktop), so the tooltip has
/// to say which one rather than hard-coding the Mac symbol.
fn shortcut_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘"
    } else {
        "Ctrl+"
    }
}

/// The selected tab's icon, and the space between it and the name.
const TAB_ICON_ON: f32 = 15.;
const TAB_FACE_GAP: f32 = 6.;

/// A tab's name as the selected cell wears it.
fn tab_name(tab: Tab) -> Div {
    div()
        .flex_none()
        .text_size(px(13.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(tab.title())
}

/// As wide as `tab`'s selected face, no height, never painted. The
/// selected cell holds one for every tab and is therefore as wide as the
/// widest face — in this language and this font, with no table of
/// measured widths to keep (the Chinese names are all two characters and
/// were always one width; a third language would need a third row of
/// such a table, and Linux draws the same names in another face).
fn tab_strut(tab: Tab) -> Div {
    h_flex()
        .flex_none()
        .h(px(0.))
        .invisible()
        .gap(px(TAB_FACE_GAP))
        .child(div().flex_none().w(px(TAB_ICON_ON)))
        .child(tab_name(tab))
}

/// Every view in a single icon row — Control Center / Stats, not a text grid.
/// Full names live on the tooltip.
fn tab_strip(state: &ZStatsAppState) -> AnyElement {
    let active = state.tab();
    // Red is for an alert that is happening: a card reported in this
    // session. The bell used to turn red for any card in the list, so a
    // record restored from this morning lit it on a day with nothing
    // wrong.
    let alerting = state.alerts().iter().any(|seen| seen.live);

    let cell = |tab: Tab| {
        let on = tab == active;
        let alarm = tab == Tab::Alerts && alerting;
        // Selected is a tint of the accent under the accent, where the
        // scheme has that state (`theme::selected`); elsewhere the
        // theme's mark as a block, or the classic chip. An alerting
        // Alerts tab is red either way: that colour is the alarm, and
        // it has to win over the theme.
        let (fill, color) = match (on, alarm) {
            (true, true) => (Some(theme::accent_wash(16)), theme::accent_light()),
            (false, true) => (None, theme::accent_light()),
            (true, false) => match (theme::selected(), theme::mark()) {
                (Some((fill, ink)), _) => (Some(fill), ink),
                (None, Some((fill, ink))) => (Some(fill), ink),
                (None, None) => (Some(theme::chip()), theme::text()),
            },
            (false, false) => (None, theme::text_dim()),
        };
        let id = tab.label();
        let title = t!(
            "tabs.shortcut",
            name = tab.title(),
            hint = shortcut_hint(),
            n = tab.index() + 1
        )
        .to_string();
        // A column, so that the selected cell can be as wide as the
        // widest thing in it (`tab_strut`) while its face stays centred.
        v_flex()
            .id(id)
            .flex_none()
            .h(px(30.))
            .items_center()
            .justify_center()
            .rounded(px(7.))
            // The rest are icons in a fixed box, spread across what the
            // selected one leaves.
            .map(|d| if on { d.px(px(12.)) } else { d.w(px(34.)) })
            .when_some(fill, |d, fill| d.bg(fill))
            .when(!on, |d| d.hover(|d| d.bg(theme::chip())))
            .tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx))
            .child(
                h_flex()
                    .items_center()
                    .gap(px(TAB_FACE_GAP))
                    .child(
                        tab_icon(tab)
                            .with_size(Size::Size(px(if on { TAB_ICON_ON } else { 16. })))
                            .text_color(Hsla::from(color)),
                    )
                    // Seven glyphs with their names behind a hover was a
                    // strip to learn by heart: a chip for Processes, a
                    // drive for the page that also holds the GPU and the
                    // battery. The page on screen wears its name, so
                    // every visit teaches one.
                    .when(on, |d| d.child(tab_name(tab).text_color(Hsla::from(color)))),
            )
            // The selected cell is as wide as the longest name, whichever
            // it wears. As wide as its own, "Apps" was 78 and "Processes"
            // 111, and the six icons shared out the difference: every
            // step along the strip moved all of them by a few points.
            // Now a step moves the icons between the two tabs and
            // nothing else.
            .when(on, |d| d.children(Tab::ALL.iter().copied().map(tab_strut)))
            .on_click(move |_, _window, cx| {
                cx.global::<ZStatsGlobalStore>()
                    .clone()
                    .update(cx, |state, cx| state.set_tab(tab, cx));
            })
    };

    v_flex()
        .h(px(TAB_STRIP_HEIGHT))
        .flex_none()
        .px(px(GUTTER))
        .pt(px(GUTTER))
        .child(
            h_flex()
                .justify_between()
                .p(px(4.))
                .rounded(px(10.))
                .bg(theme::trough())
                .children(Tab::ALL.iter().copied().map(cell)),
        )
        .into_any_element()
}

/// The scrolling body. Each tab renders a stack of cards.
fn content(state: &ZStatsAppState) -> AnyElement {
    let body = match state.tab() {
        Tab::Overview => overview::render(state),
        Tab::Apps => apps::render(state),
        Tab::Processes => processes::render(state),
        // One tab for the machine's physical substrate: volumes, the
        // physical drives under them, the GPU, then temperatures and the
        // battery. Volumes answer "is it full" and drives "is it slow" —
        // two cards side by side, never a joined table, because APFS puts
        // several volumes on one drive. Every renderer returns a card
        // stack, so merging is concatenation — no layout marriage.
        Tab::Hardware => {
            let mut cards = disk::render(state);
            cards.extend(drives::render(state));
            cards.extend(gpu::render(state));
            cards.extend(sensors::render(state));
            cards
        }
        // Who is moving bytes, then who is listening, then the
        // interfaces. Traffic is why the tab was opened. The listener
        // list is the other thing nothing else in the panel shows. The
        // interface card splits a throughput Overview already carries,
        // and on a quiet machine it is an empty state — which used to
        // be the first thing the tab said. Same concatenation as
        // Hardware: each renderer returns its own card stack.
        Tab::Net => {
            let mut cards = traffic::render(state);
            cards.extend(listening::render(state));
            cards.extend(net::render(state));
            cards
        }
        Tab::Alerts => alerts::render(state),
        Tab::History => history::render(state),
    };

    let tab = state.tab();
    div()
        // Keyed per tab, or one shared id would carry the previous tab's
        // offset over to the next. The id alone does not remember anything
        // though — gpui drops element state it did not paint, and only the
        // active tab is ever painted — so the handle comes from the store.
        .id(("panel-body", tab.index()))
        .track_scroll(&gpui_ui::scroll_handle(tab))
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(GUTTER))
        .pt(px(CONTENT_TOP_PAD))
        .pb(px(BODY_BOTTOM_PAD))
        .child(v_flex().gap(px(CARD_GAP)).children(body))
        .into_any_element()
}

/// The project page. The About page links it; the footer used to.
pub(super) const REPO_URL: &str = "https://github.com/vicanso/zstats.app";

/// The footer's keep-awake control: a cup that opens a menu of lengths.
///
/// A type of its own because gpui-kit's dropdown wants a trigger it can
/// mark open ([`Selectable`]) and implements that for its own `Button`
/// only. A `Button` here would be the one footer control wearing the
/// kit's size, ink and hover fill — its icon size is derived, not set —
/// beside four hand-built neighbours; this is those four's box with the
/// two traits the dropdown asks for.
#[derive(IntoElement)]
struct AwakeTrigger {
    base: Stateful<Div>,
    /// How long the running hold has left ("42m", "1h 58m"); `None`
    /// with no hold. While there is one the cup is lit the way a set
    /// pin is, and wears this beside it.
    left: Option<String>,
    /// Its menu is open. Set by the dropdown, not by us.
    open: bool,
    tip: SharedString,
}

impl Styled for AwakeTrigger {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for AwakeTrigger {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl Selectable for AwakeTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.open = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.open
    }
}

impl DropdownMenu for AwakeTrigger {}

impl RenderOnce for AwakeTrigger {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let (left, open, tip) = (self.left, self.open, self.tip);
        let held = left.is_some();
        self.base
            .flex()
            .flex_none()
            .h(px(FOOTER_BUTTON))
            .min_w(px(32.))
            .items_center()
            .justify_center()
            .gap(px(4.))
            .px(px(8.))
            .rounded(px(7.))
            // Lit while it holds; the hover fill kept while its menu is
            // open, so the menu visibly belongs to this button.
            .when(held, |d| d.bg(theme::chip()))
            .when(open && !held, |d| d.bg(theme::surface_raised()))
            // No tooltip under an open menu: it would come up over the
            // items it describes.
            .when(!open, |d| d.tooltip(widgets::wrap_tooltip(tip)))
            .hover(|d| d.bg(theme::surface_raised()))
            .child(
                Icon::from(CustomIconName::Coffee)
                    .with_size(Size::Size(px(16.)))
                    .text_color(Hsla::from(if held {
                        theme::text()
                    } else {
                        theme::text_dim()
                    })),
            )
            // The time left, beside the cup. The tooltip and the menu
            // say *until when*, and both take a hover or a click to
            // read; what someone glancing at a lit cup wants is whether
            // it will last the job, which is a length. Held to the
            // icon's line so the footer row keeps its height, and in
            // the figures' face so a minute ticking over does not move
            // the four buttons beside it.
            .children(left.map(|left| {
                div()
                    .pr(px(2.))
                    .font_features(font::tnum())
                    .text_size(px(10.))
                    .line_height(px(14.))
                    .text_color(theme::text())
                    .child(left)
            }))
    }
}

/// A header control that opens a menu: the sort chips and History's
/// title. [`AwakeTrigger`]'s box, for a control that is not the cup.
///
/// These three cycled on a click until 2026-10-12 — one button, the
/// next ordering — which spends no width but hides what the other
/// orderings are, costs up to three clicks to go back one, and on
/// History wore a chevron that promised the menu it did not open. The
/// menu spends the same width. Give it an id keyed by
/// `panel_visit`, for the cup's reason: a kit dropdown closes only
/// from inside its window, and the panel is hidden rather than closed.
#[derive(IntoElement)]
pub(super) struct MenuTrigger {
    base: Stateful<Div>,
    /// Its menu is open. Set by the dropdown, not by us.
    open: bool,
    /// Worn as a chip, outlined like the "All" chip beside it, with a
    /// chevron after the label ([`MenuTrigger::chip`]).
    chip: bool,
    tip: SharedString,
    label: AnyElement,
}

pub(super) fn menu_trigger(
    id: impl Into<ElementId>,
    tip: impl Into<SharedString>,
    label: impl IntoElement,
) -> MenuTrigger {
    MenuTrigger {
        base: div().id(id),
        open: false,
        chip: false,
        tip: tip.into(),
        label: label.into_any_element(),
    }
}

impl MenuTrigger {
    /// The sort controls' form: the outline of the "All" chip beside
    /// them and a chevron after the label. "↓ CPU" as bare words opened
    /// a menu without saying it would, beside a chip that looked like
    /// the one thing in the header to press (2026-10-16). History's
    /// title stays words: it is the card's title, and wears its own
    /// chevron.
    pub(super) fn chip(mut self) -> Self {
        self.chip = true;
        self
    }
}

impl Styled for MenuTrigger {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for MenuTrigger {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl Selectable for MenuTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.open = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.open
    }
}

impl DropdownMenu for MenuTrigger {}

impl RenderOnce for MenuTrigger {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let (open, tip, chip) = (self.open, self.tip, self.chip);
        self.base
            .flex()
            .items_center()
            .map(|d| {
                if chip {
                    d.gap(px(3.))
                        .rounded_full()
                        .border_1()
                        .border_color(if open {
                            theme::border()
                        } else {
                            theme::border_subtle()
                        })
                } else {
                    d.rounded(px(4.))
                }
            })
            // The hover fill kept while the menu is open, and no tooltip
            // under it — as on the cup.
            .when(open, |d| d.bg(theme::surface_raised()))
            .when(!open, |d| d.tooltip(widgets::wrap_tooltip(tip)))
            .hover(move |d| {
                let d = d.bg(theme::surface_raised());
                if chip {
                    d.border_color(theme::border())
                } else {
                    d
                }
            })
            .child(self.label)
            .when(chip, |d| {
                d.child(
                    Icon::new(IconName::ChevronDown)
                        .with_size(Size::Size(px(8.)))
                        .text_color(Hsla::from(theme::text_dim())),
                )
            })
    }
}

/// A length as the menu words it. Whole words rather than the chips'
/// "30m": a menu has the width, and under "Keep awake for" a bare
/// "30m" reads as a code.
fn hold_label(minutes: u32) -> String {
    match minutes {
        m if m < 60 => t!("common.keep_awake_minutes", n = m).to_string(),
        60 => i18n::tr("common.keep_awake_hour"),
        m => t!("common.keep_awake_hours", n = m / 60).to_string(),
    }
}

/// Keep the Mac awake for a while (`awake.rs`): click the cup, pick a
/// length. Here rather than on the Interface page, where it was a row of
/// chips: it is an act with an end, not a setting, and the moment
/// someone wants it is the moment they are looking at the panel — not
/// two clicks into a window about preferences. The cup is always there
/// (a control that appears only once it is on cannot be how it is turned
/// on), lit while a hold runs with the time left beside it, and its
/// tooltip then says until when; the menu's first line says the same,
/// the running length wears the check, and "Off" ends it. The tooltip also carries what a hold does not
/// stop — the lid, a lock or sleep someone asks for — because each
/// otherwise reads as a bug.
fn keep_awake_menu(state: &ZStatsAppState) -> impl IntoElement {
    let hold = awake::held();
    let mac = cfg!(target_os = "macos");
    let tip = match hold {
        Some(hold) => t!(
            if mac {
                "common.keep_awake_on"
            } else {
                "common.keep_awake_on_linux"
            },
            time = format::clock(hold.until)
        )
        .to_string(),
        None => i18n::tr(if mac {
            "common.keep_awake"
        } else {
            "common.keep_awake_linux"
        }),
    };
    AwakeTrigger {
        // Keyed by the visit, so a menu left open by a hide is not
        // still open when the panel comes back (`panel_visit`).
        base: div().id(("keep-awake", state.panel_visit())),
        left: hold.map(|hold| format::uptime(u64::from(hold.minutes_left(SystemTime::now())) * 60)),
        open: false,
        tip: tip.into(),
    }
    // Upward and left-aligned: the footer is the panel's bottom edge
    // and the cup sits in its left-hand group.
    .dropdown_menu_with_anchor(Anchor::BottomLeft, |menu, _, _| {
        // Read when the menu opens, not when the footer was built.
        let hold = awake::held();
        let current = hold.map_or(0, |hold| hold.minutes);
        let heading = match hold {
            Some(hold) => {
                t!("common.keep_awake_until", time = format::clock(hold.until)).to_string()
            }
            None => i18n::tr("common.keep_awake_for"),
        };
        let pick = |label: String, minutes: u32| {
            menu_text(label)
                .checked(minutes == current)
                .on_click(move |_, _, cx| crate::set_keep_awake(minutes, cx))
        };
        awake::HOLD_MINUTES
            .iter()
            .fold(
                menu.item(menu_text(heading).disabled(true)),
                |menu, &minutes| menu.item(pick(hold_label(minutes), minutes)),
            )
            .separator()
            .item(pick(i18n::tr("common.keep_awake_off"), 0))
    })
}

/// A menu line at the panel's own type size. gpui-kit's menu items are
/// 14px with no setter for it, which beside the panel's 10–12px read as
/// a menu from a larger app — the same reason [`widgets::wrap_tooltip`]
/// does not use the kit's default tooltip. An item built from an element
/// carries its own text; a disabled one is the kit's muted heading.
pub(super) fn menu_text(label: String) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| div().text_size(px(MENU_PT)).child(label.clone()))
}

/// Type size of the footer menu's lines — the panel's row text.
const MENU_PT: f32 = 12.;

/// Height of a footer button: the tab strip's cell.
const FOOTER_BUTTON: f32 = 30.;

/// One of the footer's icon buttons: a 32×30 box, the icon at 16.
fn footer_button(id: &'static str, icon: Icon, lit: bool) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .w(px(32.))
        .h(px(FOOTER_BUTTON))
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .when(lit, |d| d.bg(theme::chip()))
        .hover(|d| d.bg(theme::surface_raised()))
        .child(
            icon.with_size(Size::Size(px(16.)))
                .text_color(Hsla::from(if lit {
                    theme::text()
                } else {
                    theme::text_dim()
                })),
        )
}

/// What the panel does to itself on the left — Pin, its form, Settings,
/// the keep-awake cup — and, alone on the right and in words, Quit.
///
/// The form toggle is here and not on the Overview page it reshapes:
/// what it changes is the panel's height, on every tab, and the long
/// form's way back used to be a line of its own above that page's
/// cards — 26 of height for one word. Its glyph is the act, not the
/// state: two chevrons pulling apart while the panel is short, closing
/// while it is long.
///
/// The five used to sit together on the right as five glyphs, the power
/// one last: the action that ends the app was the same size and the
/// same grey as the one that pins the panel, one slot away from it.
/// The GitHub button that sat among them is on the About page now — a
/// link out of the app is not something the panel is for, and it took a
/// place in a row of things the panel does.
fn footer(state: &ZStatsAppState) -> AnyElement {
    let nudge = state.update_nudge().map(str::to_string);
    let pinned = prefs::pinned();
    let pin_tip = i18n::tr(if pinned { "common.unpin" } else { "common.pin" });
    // Settings lives in its own window, not a tab: a session should not
    // be cut short by the popover auto-hiding on focus loss.
    //
    // One dot, two possible reasons — a second dot at this size would
    // be noise, and both answers are the same act (open settings). The
    // tooltip is what says which.
    let template_nudge = state.template_nudge();
    let settings_tip = match (&nudge, template_nudge) {
        (Some(v), true) => t!("config.both_nudge_tip", v = v).to_string(),
        // The dot's words: a silent check found a newer release; the
        // About page has the download.
        (Some(v), false) => t!("config.update_nudge_tip", v = v).to_string(),
        (None, true) => i18n::tr("config.template_nudge_tip"),
        (None, false) => i18n::tr("tabs.settings"),
    };
    let dotted = nudge.is_some() || template_nudge;
    // "Quit" alone under a power glyph, beside the cup that keeps the
    // Mac awake, read as something about the Mac.
    let quit_tip = i18n::tr("common.quit_app");
    let detail = prefs::overview_detail();
    let (form_icon, form_tip) = if detail {
        (CustomIconName::Collapse, "overview.collapse_tip")
    } else {
        (CustomIconName::Expand, "overview.expand_tip")
    };
    let form_tip = i18n::tr(form_tip);
    h_flex()
        .h(px(FOOTER_HEIGHT))
        .flex_none()
        .items_center()
        .justify_between()
        .px(px(8.))
        .border_t(px(1.))
        .border_color(theme::border_subtle())
        .child(
            h_flex()
                .items_center()
                .gap(px(4.))
                .child(
                    // Pin only stops auto-hide — a tray click still
                    // toggles, and this is not a second window model.
                    footer_button("pin", Icon::from(CustomIconName::Pin), pinned)
                        .tooltip(move |window, cx| Tooltip::new(pin_tip.clone()).build(window, cx))
                        .on_click(|_, _window, cx| {
                            prefs::set_pinned(!prefs::pinned());
                            cx.global::<ZStatsGlobalStore>()
                                .clone()
                                .update(cx, |_, cx| cx.notify());
                        }),
                )
                .child(
                    footer_button("panel-form", Icon::from(form_icon), false)
                        .tooltip(widgets::wrap_tooltip(form_tip))
                        .on_click(move |_, _window, cx| {
                            crate::set_overview_detail_pref(!detail, cx)
                        }),
                )
                .child(
                    footer_button("settings", Icon::new(IconName::Settings2), false)
                        .relative()
                        .tooltip(move |window, cx| {
                            Tooltip::new(settings_tip.clone()).build(window, cx)
                        })
                        // The nudge dot: paint only, no layout — the
                        // gear must not shift when it appears.
                        .when(dotted, |d| {
                            d.child(
                                div()
                                    .absolute()
                                    .top(px(6.))
                                    .right(px(6.))
                                    .size(px(6.))
                                    .rounded_full()
                                    .bg(Hsla::from(theme::accent())),
                            )
                        })
                        .on_click(|_, _window, cx| crate::open_settings_window(cx)),
                )
                .child(keep_awake_menu(state)),
        )
        .child(
            h_flex()
                .id("quit")
                .flex_none()
                .h(px(FOOTER_BUTTON))
                .items_center()
                .gap(px(6.))
                .px(px(10.))
                .rounded(px(7.))
                .tooltip(move |window, cx| Tooltip::new(quit_tip.clone()).build(window, cx))
                .hover(|d| d.bg(theme::surface_raised()))
                .child(
                    Icon::from(CustomIconName::Power)
                        .with_size(Size::Size(px(14.)))
                        .text_color(Hsla::from(theme::text_dim())),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::text_dim())
                        .child(i18n::tr("common.quit")),
                )
                .on_click(|_, _window, cx| cx.quit()),
        )
        .into_any_element()
}

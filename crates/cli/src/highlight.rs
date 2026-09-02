//! Background highlighting of the entries a tracked account is behind.
//!
//! Every renderer that lists exchange activity - traced events, order books,
//! trades - can be handed a [`Highlights`] set, which paints the entries of the
//! accounts it holds in that account's own background colour so they stand out
//! of the surrounding output.

use colored::{Color, Colorize};
use perpl_sdk::{
    abi::dex::Exchange::ExchangeEvents,
    state::{Order, OrderHighlight, StateEvents},
    types,
};

/// Dark ink, used on every background of the palette below. A near-black
/// true colour rather than [`Color::Black`], which resolves to the terminal
/// theme's own black and can vanish on a dark background.
const INK: Color = Color::TrueColor { r: 16, g: 16, b: 20 };

/// Ink for an entry that also carries a warning - an expired order, say - kept
/// legible on the same backgrounds.
const WARN_INK: Color = Color::TrueColor { r: 150, g: 0, b: 0 };

/// Background the single account of `--highlight` is painted in. Deliberately
/// outside [`MM_PALETTE`], so the tracked account never collides with a market
/// maker in `show mms`.
const TRACKED: Paint = Paint::new(255, 215, 0);

/// Distinct backgrounds handed out to market makers in order. All light enough
/// to carry [`INK`], and far enough apart to stay distinguishable on both light
/// and dark terminals.
const MM_PALETTE: [Paint; 10] = [
    Paint::new(126, 190, 255), // blue
    Paint::new(147, 219, 141), // green
    Paint::new(240, 150, 210), // pink
    Paint::new(255, 174, 122), // orange
    Paint::new(160, 224, 220), // teal
    Paint::new(197, 174, 255), // lavender
    Paint::new(226, 226, 140), // olive
    Paint::new(255, 160, 155), // salmon
    Paint::new(170, 205, 170), // sage
    Paint::new(205, 205, 215), // slate
];

/// A background colour an entry is painted in, always paired with an ink that
/// stays readable on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Paint {
    background: Color,
}

impl Paint {
    const fn new(r: u8, g: u8, b: u8) -> Self { Self { background: Color::TrueColor { r, g, b } } }

    /// Paints `text` on this background.
    pub(crate) fn apply(&self, text: &str) -> String { self.paint(text, INK) }

    /// Paints `text` on this background in the warning ink, for an entry that
    /// the surrounding renderer would otherwise have coloured red.
    pub(crate) fn apply_warning(&self, text: &str) -> String { self.paint(text, WARN_INK) }

    fn paint(&self, text: &str, ink: Color) -> String {
        // Any styling the caller already applied is dropped: its reset would
        // otherwise end the background part way through the entry
        strip_ansi(text)
            .on_color(self.background)
            .color(ink)
            .bold()
            .to_string()
    }
}

/// Accounts to highlight, each with the colour its entries are painted in.
///
/// An empty set - the default - leaves every renderer's own styling untouched,
/// so renderers can take one unconditionally.
#[derive(Clone, Debug, Default)]
pub(crate) struct Highlights {
    accounts: Vec<(types::AccountId, Paint)>,
}

impl Highlights {
    /// Adds the single account `--highlight` follows, under its reserved
    /// colour. An account that already has one - a market maker whose colour
    /// the legend advertises - keeps it.
    pub(crate) fn track(&mut self, account_id: types::AccountId) {
        if self.paint_of(account_id).is_none() {
            self.accounts.push((account_id, TRACKED));
        }
    }

    /// Adds `account_id` under the next unused colour of [`MM_PALETTE`],
    /// keeping the colour it already has if it is highlighted twice. The
    /// palette wraps around once exhausted.
    pub(crate) fn add(&mut self, account_id: types::AccountId) -> Paint {
        if let Some(paint) = self.paint_of(account_id) {
            return paint;
        }
        let paint = MM_PALETTE[self.accounts.len() % MM_PALETTE.len()];
        self.accounts.push((account_id, paint));
        paint
    }

    pub(crate) fn is_empty(&self) -> bool { self.accounts.is_empty() }

    /// Every highlighted account with its colour, in the order they were
    /// added - the order a legend should list them in.
    pub(crate) fn entries(&self) -> impl Iterator<Item = (types::AccountId, Paint)> + '_ {
        self.accounts.iter().copied()
    }

    /// Colour `account_id` is painted in, `None` when it is not highlighted.
    pub(crate) fn paint_of(&self, account_id: types::AccountId) -> Option<Paint> {
        self.accounts
            .iter()
            .find(|(id, _)| *id == account_id)
            .map(|(_, paint)| *paint)
    }

    /// Paints `line` if `event` names a highlighted account, otherwise returns
    /// it unchanged.
    pub(crate) fn raw_event(&self, event: &ExchangeEvents, line: String) -> String {
        if self.is_empty() {
            return line;
        }
        let debug = format!("{:?}", event);
        match raw_event_accounts(event, &debug).find_map(|id| self.paint_of(id)) {
            Some(paint) => paint.apply(&line),
            None => line,
        }
    }

    /// Paints `line` if `event` concerns a highlighted account, otherwise
    /// returns it unchanged.
    pub(crate) fn state_event(&self, event: &StateEvents, line: String) -> String {
        if self.is_empty() {
            return line;
        }
        match state_event_accounts(event)
            .into_iter()
            .find_map(|id| self.paint_of(id))
        {
            Some(paint) => paint.apply(&line),
            None => line,
        }
    }

    /// Paints `line` if `account_id` is highlighted, otherwise returns it
    /// unchanged.
    pub(crate) fn account(&self, account_id: types::AccountId, line: String) -> String {
        match self.paint_of(account_id) {
            Some(paint) => paint.apply(&line),
            None => line,
        }
    }
}

impl OrderHighlight for Highlights {
    fn highlight(&self, order: &Order, rendered: &str) -> Option<String> {
        self.paint_of(order.account_id()).map(|paint| {
            if order.is_expired() { paint.apply_warning(rendered) } else { paint.apply(rendered) }
        })
    }
}

/// Account IDs a state event concerns.
fn state_event_accounts(event: &StateEvents) -> Vec<types::AccountId> {
    match event {
        StateEvents::Account(e) => vec![e.account_id],
        StateEvents::Error(e) => vec![e.account_id],
        StateEvents::Order(e) => vec![e.account_id],
        StateEvents::Position(e) => vec![e.account_id],
        StateEvents::Trade(trade) => std::iter::once(trade.taker_account_id)
            .chain(trade.maker_fills.iter().map(|f| f.maker_account_id))
            .collect(),
        StateEvents::Exchange(_) | StateEvents::Perpetual(_) => vec![],
    }
}

/// Account IDs named by a raw exchange event, whose `debug` rendering the
/// caller has already produced.
///
/// The exchange names accounts under a handful of field names spread over
/// ~200 event variants, far too many to match on individually - so the debug
/// output is scanned for them instead. `accountId`, `posAccountId` and
/// `recyclerAccountId` all share the [`ACCOUNT_ID_FIELDS`] suffixes;
/// `AccountCreated`, which reports the new ID as a bare `id`, is the one
/// variant that has to be read directly.
fn raw_event_accounts<'a>(
    event: &ExchangeEvents,
    debug: &'a str,
) -> impl Iterator<Item = types::AccountId> + 'a {
    let created = match event {
        ExchangeEvents::AccountCreated(e) => Some(e.id.to::<types::AccountId>()),
        _ => None,
    };
    created.into_iter().chain(
        ACCOUNT_ID_FIELDS
            .iter()
            .flat_map(move |field| debug.match_indices(field))
            .filter_map(|(idx, field)| {
                debug[idx + field.len()..]
                    .split(|c: char| !c.is_ascii_digit())
                    .next()
                    .filter(|digits| !digits.is_empty())
                    .and_then(|digits| digits.parse().ok())
            }),
    )
}

/// Debug-rendered field names that carry an account ID. `ccountId` is a suffix
/// on purpose: it matches `accountId`, `posAccountId` and `recyclerAccountId`
/// alike, and the trailing separator keeps it from matching a longer number.
const ACCOUNT_ID_FIELDS: [&str; 2] = ["ccountId: ", "liquidatorId: "];

/// Drops ANSI escape sequences from `text`, so a highlight can repaint an entry
/// that is already styled without the old styling's reset cutting the new
/// background short.
fn strip_ansi(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            plain.push(ch);
            continue;
        }
        // A CSI sequence runs until its final byte in `@`..=`~`; every other
        // escape is the two characters already consumed
        if chars.next() == Some('[') {
            for ch in chars.by_ref() {
                if ('@'..='~').contains(&ch) {
                    break;
                }
            }
        }
    }
    plain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_nested_styling_before_painting() {
        // `colored` emits nothing when it decides the output is not a terminal,
        // which a test run always is not
        colored::control::set_override(true);
        let styled = "resting".red().bold().to_string();
        let painted = TRACKED.apply(&styled);
        assert!(painted.contains("resting"));
        // A single reset, at the very end, so the background covers the entry
        assert_eq!(painted.matches("\u{1b}[0m").count(), 1);
        assert!(painted.ends_with("\u{1b}[0m"));
    }

    #[test]
    fn scans_debug_output_for_account_ids() {
        let debug =
            "MakerOrderFilledV2 { perpId: 3, accountId: 42, recyclerAccountId: 7, orderId: 421 }";
        let found: Vec<_> = ACCOUNT_ID_FIELDS
            .iter()
            .flat_map(|field| debug.match_indices(field))
            .filter_map(|(idx, field)| {
                debug[idx + field.len()..]
                    .split(|c: char| !c.is_ascii_digit())
                    .next()
                    .and_then(|digits| digits.parse::<types::AccountId>().ok())
            })
            .collect();
        assert_eq!(found, vec![42, 7]);
    }

    #[test]
    fn hands_out_a_distinct_colour_per_account() {
        let mut highlights = Highlights::default();
        let first = highlights.add(1);
        let second = highlights.add(2);
        assert_ne!(first, second);
        // Adding an account twice keeps the colour it already has
        assert_eq!(highlights.add(1), first);
        assert_eq!(highlights.paint_of(2), Some(second));
        assert_eq!(highlights.paint_of(3), None);
    }
}

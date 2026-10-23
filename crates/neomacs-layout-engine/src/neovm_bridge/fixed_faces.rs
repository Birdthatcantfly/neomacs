//! Fixed face-name producers for bridge parameter construction.
//! Only native epoch/ID cells are shared. Each handle resolves lazily at the
//! original lookup branch; no face value, Lisp heap object or membership proof
//! is cached. Existing caller order and skipped branches remain authoritative.

use super::{FaceTable, NeoFace};
use neovm_core::emacs_core::intern::EpochCachedSymbol;

#[derive(Clone, Copy, Debug, strum::IntoStaticStr, strum::VariantArray)]
#[strum(serialize_all = "kebab-case")]
pub(super) enum FixedFace {
    Default,
    Cursor,
    VerticalBorder,
    WindowDivider,
    WindowDividerFirstPixel,
    WindowDividerLastPixel,
    TrailingWhitespace,
    FillColumnIndicator,
    EscapeGlyph,
    NobreakSpace,
    GlyphlessChar,
}

static DEFAULT: EpochCachedSymbol = EpochCachedSymbol::new("default");
static CURSOR: EpochCachedSymbol = EpochCachedSymbol::new("cursor");
static VERTICAL_BORDER: EpochCachedSymbol = EpochCachedSymbol::new("vertical-border");
static WINDOW_DIVIDER: EpochCachedSymbol = EpochCachedSymbol::new("window-divider");
static WINDOW_DIVIDER_FIRST_PIXEL: EpochCachedSymbol =
    EpochCachedSymbol::new("window-divider-first-pixel");
static WINDOW_DIVIDER_LAST_PIXEL: EpochCachedSymbol =
    EpochCachedSymbol::new("window-divider-last-pixel");
static TRAILING_WHITESPACE: EpochCachedSymbol = EpochCachedSymbol::new("trailing-whitespace");
static FILL_COLUMN_INDICATOR: EpochCachedSymbol = EpochCachedSymbol::new("fill-column-indicator");
static ESCAPE_GLYPH: EpochCachedSymbol = EpochCachedSymbol::new("escape-glyph");
static NOBREAK_SPACE: EpochCachedSymbol = EpochCachedSymbol::new("nobreak-space");
static GLYPHLESS_CHAR: EpochCachedSymbol = EpochCachedSymbol::new("glyphless-char");

impl FixedFace {
    #[cfg(test)]
    pub(super) fn name(self) -> &'static str {
        self.into()
    }

    fn handle(self) -> &'static EpochCachedSymbol {
        match self {
            Self::Default => &DEFAULT,
            Self::Cursor => &CURSOR,
            Self::VerticalBorder => &VERTICAL_BORDER,
            Self::WindowDivider => &WINDOW_DIVIDER,
            Self::WindowDividerFirstPixel => &WINDOW_DIVIDER_FIRST_PIXEL,
            Self::WindowDividerLastPixel => &WINDOW_DIVIDER_LAST_PIXEL,
            Self::TrailingWhitespace => &TRAILING_WHITESPACE,
            Self::FillColumnIndicator => &FILL_COLUMN_INDICATOR,
            Self::EscapeGlyph => &ESCAPE_GLYPH,
            Self::NobreakSpace => &NOBREAK_SPACE,
            Self::GlyphlessChar => &GLYPHLESS_CHAR,
        }
    }

    pub(super) fn resolve(self, table: &FaceTable) -> NeoFace {
        // Value::symbol(handle) performs the sole old constructor charge and
        // validates the current epoch at this exact original lookup boundary.
        table.resolve_fixed_symbol(self.handle())
    }
}

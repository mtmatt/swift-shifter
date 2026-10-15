// Swift Shifter's stand-in for the @preview/xarrow package used by mitex's
// LaTeX spec (\xrightarrow and friends): an arrow stretched to fit the text
// above it.
#let xarrow(sym: sym.arrow.r, body) = $stretch(#sym)^#body$

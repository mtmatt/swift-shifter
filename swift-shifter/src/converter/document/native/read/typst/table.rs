//! Tables and grids: their parts, by any name (see [`TABLE_PARTS`]), and
//! their cells, in rows as typst places them (see [`grid_rows`]).

use typst::syntax::ast::{self, AstNode, Expr};

use super::names::Name;
use super::wrapper::Presets;
use super::{Args, MAX_NESTING, Reader, join_run, unparenthesized};
use crate::converter::document::native::ast::{Align, Inline, Table};

/// The parts of a table or grid, called as `table.header(…)` and so on.
pub(super) const TABLE_PARTS: &[&str] = &["header", "footer", "cell", "hline", "vline"];
/// The most columns a table is read with (see [`Reader::table`]).
pub(super) const MAX_COLUMNS: usize = 1000;
/// The most cells the tables of a document grow by past the cells they're
/// given: for their cells' spans (see [`grid_rows`]), and for columns left
/// empty at their end (see [`Reader::table`]). With [`MAX_COLUMNS`], no
/// short source (`columns: 99999999`, `rowspan: 99999999`, `#table(columns:
/// 1000, [a])`, again and again) makes the reader build huge tables.
pub(super) const MAX_GROWN_CELLS: usize = 1 << 20;

/// What a table holds, in order (see [`Reader::table`]).
enum Piece {
    Cell(Cell),
    /// A header's cells.
    Header(Vec<Cell>),
    /// A footer's cells.
    Footer(Vec<Cell>),
}

/// A table cell: its content and the columns and rows it spans (see
/// [`grid_rows`]).
#[derive(Clone)]
pub(super) struct Cell {
    content: Vec<Inline>,
    colspan: usize,
    rowspan: usize,
}

impl Reader {
    /// `#table(columns: …, align: …, table.header(…), [cell], …,
    /// table.footer(…))`, without its lines; `None` if it has no cells. As
    /// typst lays it out, a header or footer fills rows of its own, and only
    /// a header that opens the table heads it.
    pub(super) fn table(&mut self, args: &Args) -> Option<Table> {
        let mut pieces = Vec::new();
        for expr in args.positional() {
            let expr = unparenthesized(expr);
            match self.table_part(expr) {
                Some("header") => pieces.push(Piece::Header(self.section(expr, args))),
                Some("footer") => pieces.push(Piece::Footer(self.section(expr, args))),
                Some("hline" | "vline") => {}
                _ => pieces.extend(self.cells(expr, args).into_iter().map(Piece::Cell)),
            }
        }
        let columns = match args.named("columns") {
            // typst's default, `()`, is one column, as is one track size.
            None | Some(Expr::Auto(_) | Expr::Numeric(_)) => 1,
            Some(Expr::Int(n)) => usize::try_from(n.get()).unwrap_or(1),
            Some(expr) => match array_items(expr) {
                Some(tracks) => tracks.len(),
                // A value the reader can't tell (`columns: cols`): as wide
                // as an opening header.
                None => match pieces.first() {
                    Some(Piece::Header(cells)) => {
                        cells.iter().map(|cell| cell.colspan.min(MAX_COLUMNS)).sum()
                    }
                    _ => 0,
                },
            },
        }
        .clamp(1, MAX_COLUMNS);
        let mut align: Vec<Align> = match args.named("align") {
            Some(expr) => match (array_items(expr), expr) {
                (Some(items), _) => items.into_iter().map(alignment).collect(),
                // An array spreading a value the reader can't tell: the
                // alignments it can.
                (None, Expr::Array(array)) => array
                    .items()
                    .map(|item| match item {
                        ast::ArrayItem::Pos(e) => alignment(e),
                        ast::ArrayItem::Spread(_) => Align::Default,
                    })
                    .collect(),
                (None, single) => vec![alignment(single); columns],
            },
            None => Vec::new(),
        };
        align.resize(columns, Align::Default);

        let mut pieces = pieces.into_iter().peekable();
        let mut header_rows = match pieces.next_if(|p| matches!(p, Piece::Header(_))) {
            Some(Piece::Header(cells)) => grid_rows(cells, columns, &mut self.grown_cells_left),
            _ => Vec::new(),
        }
        .into_iter();
        let mut header = header_rows.next().unwrap_or_default();
        let mut rows: Vec<_> = header_rows.collect();
        let mut body = Vec::new();
        for piece in pieces {
            match piece {
                Piece::Cell(cell) => body.push(cell),
                Piece::Header(cells) | Piece::Footer(cells) => {
                    let body_so_far = std::mem::take(&mut body);
                    rows.extend(grid_rows(body_so_far, columns, &mut self.grown_cells_left));
                    rows.extend(grid_rows(cells, columns, &mut self.grown_cells_left));
                }
            }
        }
        rows.extend(grid_rows(body, columns, &mut self.grown_cells_left));
        if header.is_empty() && rows.is_empty() {
            return None;
        }
        // Columns empty in every row at the table's end (a form's column to
        // fill in, or a table given fewer cells than columns) are as many
        // cells grown; past what's left to grow by, they're left out.
        let empty = |x: usize| {
            header.get(x).is_none_or(Vec::is_empty) && rows.iter().all(|row| row[x].is_empty())
        };
        let empty_columns = (1..columns).rev().take_while(|&x| empty(x)).count();
        let cost = empty_columns * (rows.len() + 1);
        match self.grown_cells_left.checked_sub(cost) {
            Some(left) => self.grown_cells_left = left,
            None => {
                let kept = columns - empty_columns;
                align.truncate(kept);
                header.truncate(kept);
                rows.iter_mut().for_each(|row| row.truncate(kept));
            }
        }
        Some(Table {
            align,
            header,
            rows,
        })
    }

    /// The cells of a table's header or footer, without its lines: those
    /// of a call, given what it's given among `given` (see
    /// [`Reader::unwrapped`]), or those of one made beforehand (`#let th =
    /// table.header(…)`, placed as `th`).
    pub(super) fn section<'a>(&mut self, expr: Expr<'a>, given: &Args<'a>) -> Vec<Cell> {
        let Expr::FuncCall(call) = expr else {
            return self.part_cells(self.resolve(expr));
        };
        let mut held = Vec::new();
        let (_, args) = self.unwrapped(call, given, &mut held);
        let mut cells = Vec::new();
        for expr in args.positional() {
            let expr = unparenthesized(expr);
            if !matches!(self.table_part(expr), Some("hline" | "vline")) {
                cells.extend(self.cells(expr, &args));
            }
        }
        cells
    }

    /// The cells `expr`, among `given`, makes in a table: one, or those of
    /// the part it names, made beforehand (`#let c = table.cell(…)`, placed
    /// as `c`), unless it calls a function the reader can't see
    /// ([`Name::Unknown`]), or a preset of one. That may make cells, a
    /// header or a line, so if what it shows (see
    /// [`Reader::custom_shown_args`]) is all given by position and holds
    /// content, each argument it shows is a cell, as a header's would be,
    /// and no text is lost. Shown anything by name (a box's `title: […]`),
    /// it's one cell of all it shows, as is a call given only strings and
    /// other values. Given nothing but settings by name, it's a line, as
    /// is a name the reader can't see, placed on its own (`toprule`).
    fn cells<'a>(&mut self, expr: Expr<'a>, given: &Args<'a>) -> Vec<Cell> {
        match expr {
            Expr::FuncCall(call) => {
                let (presets, callee) = Presets::of(self.resolve(call.callee()));
                if !matches!(callee, Name::Unknown) {
                    return vec![self.cell(expr, given)];
                }
                let args = presets.give(&Args::new(call.args()));
                let shown = self.custom_shown_args(&args, None);
                if shown.iter().all(|&(name, _)| name.is_none())
                    && shown.iter().any(|&(_, expr)| self.is_content(expr))
                {
                    return shown
                        .into_iter()
                        .map(|(_, expr)| self.cell(expr, given))
                        .collect();
                }
                if shown.is_empty() && args.positional().next().is_none() {
                    return Vec::new();
                }
            }
            Expr::Ident(_) | Expr::FieldAccess(_) => match self.resolve(expr) {
                part @ Name::Part { .. } => return self.part_cells(part),
                Name::Unknown => return Vec::new(),
                _ => {}
            },
            _ => {}
        }
        vec![self.cell(expr, given)]
    }

    /// The cells of `part`, a table's header, footer or cell made beforehand
    /// (see [`Name::Part`]), placed here: its call read here, in the file it
    /// was made in and with the names bound where it was made; anything
    /// else has none. Each reading pays for its call out of what
    /// the reader may spend unwrapping (see
    /// [`super::wrapper::MAX_UNWRAPPED`]), so that parts placing parts
    /// twice, again and again, can't multiply it, and is a level as deep as
    /// calls are read, so that a part placing itself (`#let h =
    /// table.header(h)`) stops there.
    fn part_cells(&mut self, part: Name) -> Vec<Cell> {
        let Name::Part {
            part,
            call,
            dir,
            file,
            places,
            names,
        } = part
        else {
            return Vec::new();
        };
        if self.nesting >= MAX_NESTING {
            return Vec::new();
        }
        let Some(left) = self.unwrapping_left.checked_sub(call.len()) else {
            self.unwrapping_left = 0;
            return Vec::new();
        };
        self.unwrapping_left = left;
        let Some(expr) = call.cast::<Expr>() else {
            return Vec::new();
        };
        let dir_here = std::mem::replace(&mut self.dir, dir);
        let file_here = std::mem::replace(&mut self.file, file);
        let places_here = std::mem::replace(&mut self.skipped_places, places);
        let kept = self.part_names.borrow()[names].clone();
        let names_here = self.names.replace(kept);
        self.nesting += 1;
        let cells = match part {
            "header" | "footer" => self.section(expr, &Args::default()),
            "cell" => vec![self.cell(expr, &Args::default())],
            // A line.
            _ => Vec::new(),
        };
        self.nesting -= 1;
        self.names.replace(names_here);
        self.skipped_places = places_here;
        self.file = file_here;
        self.dir = dir_here;
        cells
    }

    /// The cell `expr`, among `given`, makes (see [`Reader::cells`]).
    pub(super) fn cell<'a>(&mut self, expr: Expr<'a>, given: &Args<'a>) -> Cell {
        // `table.cell(colspan: 2, …)[content]`, by any name. A wrapper may
        // give it more than its body (see `Wrapper::arguments`): it shows
        // all it's given.
        if let Expr::FuncCall(call) = expr
            && self.table_part(expr) == Some("cell")
        {
            let mut held = Vec::new();
            let (_, args) = self.unwrapped(call, given, &mut held);
            let mut content = Vec::new();
            for expr in args.positional() {
                let more = self.inlines_of(expr);
                join_run(&mut content, more);
            }
            return Cell {
                colspan: span(&args, "colspan"),
                rowspan: span(&args, "rowspan"),
                content,
            };
        }
        Cell {
            content: self.inlines_of(expr),
            colspan: 1,
            rowspan: 1,
        }
    }

    /// Which of a table's parts ([`TABLE_PARTS`]) `expr` is, if any: a
    /// call to `table.header`, `grid.hline` and so on, by any name, or a
    /// name for a part made beforehand.
    pub(super) fn table_part(&self, expr: Expr) -> Option<&'static str> {
        match expr {
            Expr::Parenthesized(inner) => self.table_part(inner.expr()),
            Expr::FuncCall(call) => match self.resolve(call.callee()) {
                Name::Function(name) => table_part(&name),
                Name::Wrapper(wrapper) => table_part(&wrapper.function),
                _ => None,
            },
            Expr::Ident(_) | Expr::FieldAccess(_) => match self.resolve(expr) {
                Name::Part { part, .. } => Some(part),
                _ => None,
            },
            _ => None,
        }
    }
}

/// Which of a table's parts ([`TABLE_PARTS`]) the function `name` is.
pub(super) fn table_part(name: &str) -> Option<&'static str> {
    let (parent, part) = name.split_once('.')?;
    if !matches!(parent, "table" | "grid") {
        return None;
    }
    TABLE_PARTS.iter().copied().find(|p| *p == part)
}

/// `cells` in rows of `columns`, as typst places them: each in the next
/// slot no cell before it covers, with the other slots a cell spans left
/// empty. Every row is full. A span past the last column or into a
/// covered slot, which typst refuses, covers the free slots it reaches.
/// Each row a span adds spends `columns` cells of `grown_cells_left`, and
/// each slot it covers in the cell's own row one; once that's spent, spans
/// add no rows and cover one column (see [`MAX_GROWN_CELLS`]).
fn grid_rows(
    cells: Vec<Cell>,
    columns: usize,
    grown_cells_left: &mut usize,
) -> Vec<Vec<Vec<Inline>>> {
    // A slot is `None` until a cell covers it.
    let mut grid: Vec<Vec<Option<Vec<Inline>>>> = Vec::new();
    let free = |grid: &[Vec<Option<Vec<Inline>>>], y: usize, x: usize| {
        grid.get(y).is_none_or(|row| row[x].is_none())
    };
    let (mut y, mut x) = (0, 0);
    for cell in cells {
        loop {
            if x == columns {
                (y, x) = (y + 1, 0);
            }
            if free(&grid, y, x) {
                break;
            }
            x += 1;
        }
        let width = (x..columns).take_while(|&x| free(&grid, y, x)).count();
        let colspan = cell.colspan.min(width);
        let colspan = match grown_cells_left.checked_sub(colspan - 1) {
            Some(left) => {
                *grown_cells_left = left;
                colspan
            }
            None => 1,
        };
        // The cell's own row, and the rows past the grid's that its span
        // adds as far as they're paid for.
        let rows = grid.len().max(y + 1);
        let bottom = y
            .saturating_add(cell.rowspan)
            .min(rows + *grown_cells_left / columns)
            .max(y + 1);
        *grown_cells_left -= bottom.saturating_sub(rows) * columns;
        if grid.len() < bottom {
            grid.resize(bottom, vec![None; columns]);
        }
        for row in &mut grid[y..bottom] {
            for slot in &mut row[x..x + colspan] {
                slot.get_or_insert_with(Vec::new);
            }
        }
        grid[y][x] = Some(cell.content);
        x += colspan;
    }
    grid.into_iter()
        .map(|row| row.into_iter().map(Option::unwrap_or_default).collect())
        .collect()
}

/// The items of the array `expr` makes, if the reader can tell them
/// without running code: `(auto, 1fr)`, `(auto,) * 3`, `3 * (auto,)` or
/// `(auto,) * 2 + (1fr,)`, through parentheses and spreads of such
/// arrays. No more than [`MAX_COLUMNS`] are kept. A chain of operators
/// nests as deep as it's long (`a + b + c` is `(a + b) + c`), and typst
/// parses one of any length, so its spine is walked along, not down the
/// stack.
pub(super) fn array_items(expr: Expr) -> Option<Vec<Expr>> {
    // What the chain's operators do to the array it starts with, the
    // outermost first.
    let mut steps = Vec::new();
    let mut expr = expr;
    let mut items = loop {
        expr = match expr {
            Expr::Binary(binary) => match (binary.op(), binary.lhs(), binary.rhs()) {
                (ast::BinOp::Add, lhs, rhs) => {
                    steps.push(ArrayStep::Add(rhs));
                    lhs
                }
                (ast::BinOp::Mul, Expr::Int(times), array)
                | (ast::BinOp::Mul, array, Expr::Int(times)) => {
                    steps.push(ArrayStep::Repeat(times.get()));
                    array
                }
                _ => return None,
            },
            Expr::Parenthesized(inner) => break array_items(inner.expr())?,
            Expr::Array(array) => break array_literal_items(array)?,
            _ => return None,
        };
    };
    for step in steps.into_iter().rev() {
        match step {
            ArrayStep::Add(array) => items.extend(array_items(array)?),
            ArrayStep::Repeat(times) => {
                let times = usize::try_from(times).ok()?;
                let len = items.len().saturating_mul(times).min(MAX_COLUMNS);
                items = items.into_iter().cycle().take(len).collect();
            }
        }
        items.truncate(MAX_COLUMNS);
    }
    items.truncate(MAX_COLUMNS);
    Some(items)
}

/// What an operator in a chain does to the array before it (see
/// [`array_items`]).
enum ArrayStep<'a> {
    /// Adds the items of this array after its own.
    Add(Expr<'a>),
    /// Repeats its items this many times.
    Repeat(i64),
}

/// The items of the array `array` writes out, its spreads' included (see
/// [`array_items`]).
fn array_literal_items(array: ast::Array) -> Option<Vec<Expr>> {
    let mut items = Vec::new();
    for item in array.items() {
        match item {
            ast::ArrayItem::Pos(item) => items.push(item),
            ast::ArrayItem::Spread(spread) => items.extend(array_items(spread.expr())?),
        }
    }
    Some(items)
}

/// `left`, `center`, `right` (also within `left + horizon`).
fn alignment(expr: Expr) -> Align {
    let text = expr.to_untyped().full_text();
    if text.contains("left") || text.contains("start") {
        Align::Left
    } else if text.contains("center") {
        Align::Center
    } else if text.contains("right") || text.contains("end") {
        Align::Right
    } else {
        Align::Default
    }
}

/// The columns or rows (`name`) a `table.cell(…)` call given `args` spans:
/// one unless it says.
fn span(args: &Args, name: &str) -> usize {
    match args.named(name) {
        Some(Expr::Int(n)) => usize::try_from(n.get()).map_or(1, |n| n.max(1)),
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::super::read;
    use super::super::tests::{one_table, strings, table_texts, text};
    use super::*;
    use crate::converter::document::native::ast::Block;

    #[test]
    fn only_table_functions_shape_a_table() {
        let doc = read(
            "#table(columns: 2, table.header([H1], [H2]), sectionheader[A], [B], \
             table.hline(), myvline[C], strong[D])\n",
        );
        assert_eq!(
            doc.blocks,
            [Block::Table(Table {
                align: vec![Align::Default; 2],
                header: vec![vec![text("H1")], vec![text("H2")]],
                rows: vec![
                    vec![vec![text("A")], vec![text("B")]],
                    vec![vec![text("C")], vec![Inline::Strong(vec![text("D")])]],
                ],
            })]
        );
    }

    #[test]
    fn headers_after_the_first_row_stay_where_typst_puts_them() {
        // As typst lays them out: a header or footer starts a row of its
        // own, and only a header that opens the table heads it.
        assert_eq!(
            table_texts(
                "#table(columns: 2, table.header([H1], [H2], [H3], [H4]), [a], [b], \
                 table.header(level: 2, [S1], [S2]), [c], [d])\n\n\
                 #table(columns: 2, [a], table.header([M1], [M2]), [c], [d], table.footer([F]))\n"
            ),
            [
                (
                    strings(&["H1", "H2"]),
                    vec![
                        strings(&["H3", "H4"]),
                        strings(&["a", "b"]),
                        strings(&["S1", "S2"]),
                        strings(&["c", "d"]),
                    ]
                ),
                (
                    Vec::new(),
                    vec![
                        strings(&["a", ""]),
                        strings(&["M1", "M2"]),
                        strings(&["c", "d"]),
                        strings(&["F", ""]),
                    ]
                ),
            ]
        );
    }

    #[test]
    fn column_specs_are_evaluated_as_typst_does() {
        let expected = (
            Vec::new(),
            vec![strings(&["a", "b", "c"]), strings(&["d", "e", "f"])],
        );
        for columns in [
            "(auto,) * 3",
            "3 * (1fr,)",
            "(auto,) * 2 + (1fr,)",
            "((auto, 1fr, 2cm))",
            "(..(auto,) * 2, 1fr)",
        ] {
            let source = format!("#table(columns: {columns}, [a], [b], [c], [d], [e], [f])\n");
            assert_eq!(
                table_texts(&source),
                std::slice::from_ref(&expected),
                "{source}"
            );
        }
        // One track, or none given, is one column.
        let one = (Vec::new(), vec![strings(&["a"]), strings(&["b"])]);
        for source in [
            "#table(columns: 1fr, [a], [b])\n",
            "#table(columns: auto, [a], [b])\n",
            "#table(columns: (), [a], [b])\n",
            "#table([a], [b])\n",
        ] {
            assert_eq!(table_texts(source), std::slice::from_ref(&one), "{source}");
        }
        assert_eq!(
            table_texts("#table(table.header([A], [B]), [a])\n"),
            [(strings(&["A"]), vec![strings(&["B"]), strings(&["a"])])]
        );
        // Alignments repeat the way columns do.
        let doc = read("#table(columns: (auto,) * 4, align: (left, right) * 2, [a])\n");
        let [Block::Table(table)] = doc.blocks.as_slice() else {
            panic!("{:?}", doc.blocks);
        };
        assert_eq!(
            table.align,
            [Align::Left, Align::Right, Align::Left, Align::Right]
        );
    }

    #[test]
    fn spanning_cells_leave_the_slots_they_cover_empty() {
        // As typst places them: each cell in the next slot no other cell
        // covers.
        let cases: [(&str, Vec<Vec<String>>); 5] = [
            (
                "#table(columns: 3, table.cell(colspan: 2)[wide], [c], [d], [e], [f])\n",
                vec![strings(&["wide", "", "c"]), strings(&["d", "e", "f"])],
            ),
            (
                "#table(columns: 3, table.cell(rowspan: 2)[tall], [b], [c], [d], [e], [f], [g])\n",
                vec![
                    strings(&["tall", "b", "c"]),
                    strings(&["", "d", "e"]),
                    strings(&["f", "g", ""]),
                ],
            ),
            (
                "#table(columns: 2, table.cell(colspan: 2, rowspan: 2)[big], [a], [b])\n",
                vec![
                    strings(&["big", ""]),
                    strings(&["", ""]),
                    strings(&["a", "b"]),
                ],
            ),
            // A cell spanning past the last row adds rows.
            (
                "#table(columns: 2, [a], table.cell(rowspan: 3)[b])\n",
                vec![strings(&["a", "b"]), strings(&["", ""]), strings(&["", ""])],
            ),
            // A span typst refuses, past the last column, covers what fits.
            (
                "#table(columns: 3, [a], [b], table.cell(colspan: 2)[wide], [c])\n",
                vec![strings(&["a", "b", "wide"]), strings(&["c", "", ""])],
            ),
        ];
        for (source, rows) in cases {
            assert_eq!(table_texts(source), [(Vec::new(), rows)], "{source}");
        }
        // Headers and footers are laid out the same way, on rows of their
        // own.
        assert_eq!(
            table_texts(
                "#table(columns: 2, table.header(table.cell(colspan: 2)[Title]), [a], [b], \
                 table.footer(table.cell(colspan: 2)[Foot]))\n\n\
                 #table(columns: 2, table.header([H1], table.cell(rowspan: 2)[H2], [H3]), \
                 [a], [b], table.footer([F]))\n"
            ),
            [
                (
                    strings(&["Title", ""]),
                    vec![strings(&["a", "b"]), strings(&["Foot", ""])]
                ),
                (
                    strings(&["H1", "H2"]),
                    vec![
                        strings(&["H3", ""]),
                        strings(&["a", "b"]),
                        strings(&["F", ""])
                    ]
                ),
            ]
        );
    }

    #[test]
    fn tables_without_cells_are_dropped() {
        for source in [
            "#table()\n",
            "#table(columns: 2)\n",
            "#grid(columns: (1fr,) * 3, gutter: 1em)\n",
            "#table(columns: 2, table.header(), table.hline())\n",
        ] {
            assert_eq!(read(source).blocks, [], "{source}");
        }
        // Empty cells are cells.
        assert_eq!(
            table_texts("#table([], [])\n"),
            [(Vec::new(), vec![vec![String::new()], vec![String::new()]])]
        );
    }

    #[test]
    fn huge_column_counts_and_spans_stay_bounded() {
        for columns in ["99999999999", "(auto,) * 99999999999"] {
            let doc = read(&format!(
                "#table(columns: {columns}, table.cell(colspan: 99999999999, \
                 rowspan: 99999999999)[a], [b])\n"
            ));
            let [Block::Table(table)] = doc.blocks.as_slice() else {
                panic!("{:?}", doc.blocks);
            };
            // At most `MAX_COLUMNS` columns, and spans add at most
            // `MAX_GROWN_CELLS` past the rows the two cells start (the
            // columns `a` spans, empty, are left out once that's spent).
            assert!(table.align.len() <= MAX_COLUMNS);
            let cells = table.rows.len() * table.align.len();
            assert!(cells <= MAX_GROWN_CELLS + 2 * MAX_COLUMNS, "{cells} cells");
            assert_eq!(table.rows[0][0], [text("a")]);
            assert_eq!(table.rows.last().unwrap()[0], [text("b")]);
        }
        // The bound is the document's: tables after it spend it keep their
        // cells but grow no more for their spans.
        let source = "#table(columns: 100, table.cell(rowspan: 99999999)[a], [b])\n\n".repeat(10);
        let doc = read(&source);
        assert_eq!(doc.blocks.len(), 10);
        let mut rows = 0;
        for block in &doc.blocks {
            let Block::Table(table) = block else {
                panic!("{block:?}");
            };
            assert_eq!(table.rows[0][..2], [vec![text("a")], vec![text("b")]]);
            rows += table.rows.len();
        }
        assert!(rows <= MAX_GROWN_CELLS / 100 + 20, "{rows} rows");

        // Columns left empty at a table's end are cells grown too: a form's
        // column to fill in is kept, but past the bound they're left out.
        assert_eq!(
            table_texts("#table(columns: 2, [Name], [], [Ann], [])\n"),
            [(
                Vec::new(),
                vec![strings(&["Name", ""]), strings(&["Ann", ""])]
            )]
        );
        let doc = read(&"#table(columns: 1000, [a])\n\n".repeat(600));
        let widths: Vec<usize> = doc
            .blocks
            .iter()
            .map(|block| match block {
                Block::Table(table) => table.align.len(),
                _ => 0,
            })
            .collect();
        assert_eq!((widths[0], widths[599]), (1000, 1));
        // So are the slots a span covers in its own row.
        let doc = read(
            &"#table(columns: 1000, ..(table.cell(colspan: 999)[a], [b]) * 500)\n\n".repeat(40),
        );
        let slots: usize = doc
            .blocks
            .iter()
            .map(|block| match block {
                Block::Table(table) => table.rows.len() * table.align.len(),
                _ => 0,
            })
            .sum();
        assert!(slots <= MAX_GROWN_CELLS + 2 * 40 * 1000, "{slots} slots");
    }

    #[test]
    fn block_content_in_cells_keeps_its_text() {
        assert_eq!(
            table_texts(
                "#table(columns: 3, align(center)[- one\n- two], quote(block: true)[Quoted], \
                 block[= Title], table(columns: 1, [i1], [i2]), \
                 block[First para.\n\nSecond para.], table.cell(align: left, strong[wide]))\n"
            ),
            [(
                Vec::new(),
                vec![
                    vec!["one two".to_string(), "Quoted".into(), "Title".into()],
                    vec![
                        "i1 i2".into(),
                        "First para. Second para.".into(),
                        "wide".into()
                    ],
                ]
            )]
        );
    }

    #[test]
    fn cells_of_calls_the_reader_cant_tell_keep_the_rows() {
        // Strings before content are settings, and a call given only
        // values is one cell, as a method call on a value is, which shows
        // nothing of what it computes with.
        assert_eq!(
            table_texts(
                "#let names = (\"Ann\", \"Bob\")\n\
                 #table(columns: 2, [Speed], qty(\"10\", \"km/h\"), badge(\"#f00\")[hot], [b], \
                 [Rev], \"abc\".rev(), upper(\"abc\").rev(), [y], \
                 datetime(year: 2024, month: 1, day: 2).display(), [z], \
                 [Who], names.join(\", \"))\n"
            ),
            one_table(&[
                &["Speed", "10 km/h"],
                &["hot", "b"],
                &["Rev", ""],
                &["", "y"],
                &["", "z"],
                &["Who", ""]
            ])
        );
        // A name the reader can't see, placed bare, is taken for a line, as
        // a call to one given nothing is.
        assert_eq!(
            table_texts(
                "#import \"@preview/booktabs:0.1.0\": *\n\
                 #table(columns: 2, toprule, [a], [b], midrule(), [c], [d], bottomrule)\n"
            ),
            one_table(&[&["a", "b"], &["c", "d"]])
        );
    }

    #[test]
    fn content_given_by_name_to_calls_the_reader_cant_tell_adds_no_cells() {
        // A box's title and footer show in its cell, with its body.
        assert_eq!(
            table_texts(
                "#import \"@preview/gentle-clues:1.0.0\": *\n\
                 #table(columns: 2, [Tip], tip(title: [Hint])[Use X], [a], [b])\n"
            ),
            one_table(&[&["Tip", "Hint Use X"], &["a", "b"]])
        );
        assert_eq!(
            table_texts(
                "#import \"@preview/showybox:2.0.4\": showybox\n\
                 #table(columns: 2, [x], showybox(title: [Title], footer: [Foot])[Body], \
                 [a], [b])\n"
            ),
            one_table(&[&["x", "Title Foot Body"], &["a", "b"]])
        );
        // Given by name alone, it's still one cell, and given content by
        // name, it's one cell however much it's given by position.
        assert_eq!(
            table_texts(
                "#import \"@preview/x:1.0.0\": *\n\
                 #table(columns: 2, clue(title: [T], footer: [F]), [a], \
                 pair(title: [T])[L][R], two([A], note: [n]))\n"
            ),
            one_table(&[&["T F", "a"], &["T L R", "A n"]])
        );
        // Content given by name is content of any form, and a string given
        // by a name for what it shows is too: it's no line, and no cell
        // after it moves.
        for (call, cell) in [
            ("showybox(title: emph[Hint])", "Hint"),
            ("showybox(title: strong[Hint], [Body])", "Hint Body"),
            ("tip(title: emph[Hint])[Use X]", "Hint Use X"),
            ("showybox(title: \"Hint\")", "Hint"),
            ("showybox(title: [T], [B1], [B2])", "T B1 B2"),
            ("tip(title: [T], \"a\", \"b\")", "T a b"),
            ("qty(\"10\", \"km/h\", note: [approx])", "10 km/h approx"),
        ] {
            let source = format!(
                "#import \"@preview/x:1.0.0\": *\n#table(columns: 2, [Tip], {call}, [a], [b])\n"
            );
            assert_eq!(
                table_texts(&source),
                one_table(&[&["Tip", cell], &["a", "b"]]),
                "{source}"
            );
        }
        // Given only settings by name, it's a line, as in booktabs-style
        // tables; given strings and content, it shows the content.
        assert_eq!(
            table_texts(
                "#import \"@preview/x:1.0.0\": *\n\
                 #table(columns: 2, toprule(stroke: 1pt), [a], [b], \
                 badge(color: \"red\")[hot], [c])\n"
            ),
            one_table(&[&["a", "b"], &["hot", "c"]])
        );
    }
}

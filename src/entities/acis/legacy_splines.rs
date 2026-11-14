//! lince (SOL-02): spline subtypes across the ACIS 4.0 / ShapeManager divide.
//!
//! The subtype blocks (`{ ... }`) of `spline-surface`, `intcurve-curve` and
//! `pcurve` records changed between the ACIS 4.0 schema of R13-R2000 drawings
//! (`exactsur`, `exactcur`, `exppc`...) and the ShapeManager one of 2007+
//! (`exact_spl_sur <version> full`, `exact_int_cur`, `exp_par_cur`...). One
//! walker reads either dialect and yields the fields of the 4.0 one, typed, so
//! they can be written as 4.0 SAT text (a ShapeManager body saved as DWG 2000)
//! or as correctly tagged 4.0 SAB (an R2000 body saved as DWG 2013+: SAT text
//! carries no numeric types, and AutoCAD rejects whole numbers where it reads
//! doubles). Grammars inferred from what AutoCAD 2027 writes when it saves the
//! same bodies both ways (`tools/solidos`); anything else is refused (`None`)
//! rather than guessed.

use super::types::{SatRecord, SatToken};

#[derive(Clone, Copy)]
enum Logical {
    /// `I` (infinite) / `F` (finite, a value follows).
    Bound,
    /// `forward` / `reversed`.
    Sense,
    /// `forward_v` / `reverse_v`.
    SenseV,
    /// `surf2` / `surf1`.
    Surface,
}

#[derive(Clone, Copy)]
enum Table {
    Closure,
    Singularity,
    Radius,
}

enum Atom {
    Raw(SatToken),
    Int(i64),
    Real(f64),
    Logical(bool, Logical),
    Enum(i64, Table),
    Id(String),
    Str(&'static str),
    Position([f64; 3]),
    Direction([f64; 3]),
    /// `{`: of a definition, with its source (record, token), or of a `ref`.
    Open(Option<(usize, usize)>),
    /// The number of a `ref`, renumbered when definitions are dropped.
    Ref(i64),
}

/// Parameter ranges and closures of a B-spline surface.
struct Net {
    range: [f64; 4],
    closure: [i64; 2],
}

struct Walker<'a> {
    tokens: &'a [SatToken],
    /// Index of the record in its body (for `Atom::Open`).
    record: usize,
    at: usize,
    /// The source is in the ShapeManager dialect (else ACIS 4.0).
    asm: bool,
    out: Vec<Atom>,
}

impl<'a> Walker<'a> {
    /// A keyword, or a counted string (a blend support names itself so).
    fn peek(&self) -> Option<&'a str> {
        let token = self.tokens.get(self.at)?;
        token.as_ident().or_else(|| token.as_string())
    }

    fn word(&mut self) -> Option<&'a str> {
        let word = self.peek()?;
        self.at += 1;
        Some(word)
    }

    fn expect(&mut self, word: &str) -> Option<()> {
        (self.word()? == word).then_some(())
    }

    /// Copies an identifier (`{`, `}`, `nubs`, `null_surface`...).
    fn id(&mut self) -> Option<&'a str> {
        let word = self.word()?;
        self.out.push(Atom::Id(word.to_string()));
        Some(word)
    }

    fn take_int(&mut self) -> Option<i64> {
        let value = self.tokens.get(self.at)?.as_integer()?;
        self.at += 1;
        Some(value)
    }

    fn int(&mut self) -> Option<i64> {
        let value = self.take_int()?;
        self.out.push(Atom::Int(value));
        Some(value)
    }

    fn take_real(&mut self) -> Option<f64> {
        let value = self.tokens.get(self.at)?.as_float()?;
        self.at += 1;
        Some(value)
    }

    fn real(&mut self) -> Option<f64> {
        let value = self.take_real()?;
        self.out.push(Atom::Real(value));
        Some(value)
    }

    fn reals(&mut self, count: usize) -> Option<()> {
        (0..count).try_for_each(|_| self.real().map(|_| ()))
    }

    fn take_logical(&mut self) -> Option<bool> {
        let token = self.tokens.get(self.at)?;
        let value = match token {
            SatToken::True | SatToken::Sab { tag: 0x0B, .. } => true,
            SatToken::False | SatToken::Sab { tag: 0x0A, .. } => false,
            _ => match token.as_ident()? {
                "I" | "T" | "forward" | "forward_v" | "surf2" => true,
                "F" | "reversed" | "reverse_v" | "reversed_v" | "surf1" => false,
                _ => return None,
            },
        };
        self.at += 1;
        Some(value)
    }

    fn logical(&mut self, kind: Logical) -> Option<bool> {
        let value = self.take_logical()?;
        self.out.push(Atom::Logical(value, kind));
        Some(value)
    }

    /// An enumeration: its number, or one of the names the readers give it.
    fn take_enum(&mut self) -> Option<i64> {
        let token = self.tokens.get(self.at)?;
        let value = match token.as_ident() {
            Some("full" | "open" | "none" | "UNEXTENDED") => 0,
            Some("closed" | "low") => 1,
            Some("periodic" | "high") => 2,
            Some("both") => 3,
            Some("no_radius") => -1,
            Some(_) => return None,
            None => token.as_integer()?,
        };
        self.at += 1;
        Some(value)
    }

    fn enumeration(&mut self, table: Table) -> Option<i64> {
        let value = self.take_enum()?;
        self.out.push(Atom::Enum(value, table));
        Some(value)
    }

    fn take_vector(&mut self) -> Option<[f64; 3]> {
        if let Some((vector, 3)) = self.tokens.get(self.at)?.coordinate_components() {
            self.at += 1;
            return Some(vector);
        }
        Some([self.take_real()?, self.take_real()?, self.take_real()?])
    }

    fn position(&mut self) -> Option<()> {
        let vector = self.take_vector()?;
        self.out.push(Atom::Position(vector));
        Some(())
    }

    fn direction(&mut self) -> Option<()> {
        let vector = self.take_vector()?;
        self.out.push(Atom::Direction(vector));
        Some(())
    }

    /// Two bounds: `I`, or `F` and its value.
    fn interval(&mut self) -> Option<()> {
        for _ in 0..2 {
            if !self.logical(Logical::Bound)? {
                self.real()?;
            }
        }
        Some(())
    }

    fn finite(&mut self, low: f64, high: f64) {
        for value in [low, high] {
            self.out.push(Atom::Logical(false, Logical::Bound));
            self.out.push(Atom::Real(value));
        }
    }

    /// Discontinuity lists: a count and that many parameters, `lists` times.
    fn discontinuities(&mut self, lists: usize) -> Option<()> {
        for _ in 0..lists {
            let count = usize::try_from(self.int()?).ok()?;
            self.reals(count)?;
        }
        Some(())
    }

    /// Knots with their multiplicities; returns (control points, first, last).
    fn knots(&mut self, degree: i64, count: i64) -> Option<(usize, f64, f64)> {
        let (mut multiplicities, mut first, mut last) = (0i64, 0.0, 0.0);
        for index in 0..count {
            last = self.real()?;
            if index == 0 {
                first = last;
            }
            multiplicities = multiplicities.checked_add(self.int()?)?;
        }
        // ACIS leaves out one repetition at each end.
        let controls = usize::try_from(multiplicities.checked_add(1)?.checked_sub(degree)?).ok()?;
        (count >= 2 && controls <= self.tokens.len()).then_some((controls, first, last))
    }

    /// `nubs|nurbs degree closure knots controls`, or `nullbs` if allowed.
    fn bs_curve(&mut self, dimensions: usize, nullable: bool) -> Option<()> {
        let rational = match self.id()? {
            "nubs" => 0,
            "nurbs" => 1,
            "nullbs" if nullable => return Some(()),
            _ => return None,
        };
        let degree = self.int()?;
        self.enumeration(Table::Closure)?;
        let count = self.int()?;
        let (controls, ..) = self.knots(degree, count)?;
        self.reals(controls.checked_mul(dimensions + rational)?)
    }

    fn bs_surface(&mut self) -> Option<Net> {
        let rational = match self.id()? {
            "nubs" => 0,
            "nurbs" => 1,
            _ => return None,
        };
        let degrees = [self.int()?, self.int()?];
        if rational == 1 {
            // Which parameters are rational: `u`, `v` or `both`.
            self.id()?;
        }
        let closure = [self.enumeration(Table::Closure)?, self.enumeration(Table::Closure)?];
        self.enumeration(Table::Singularity)?;
        self.enumeration(Table::Singularity)?;
        let counts = [self.int()?, self.int()?];
        let (controls_u, u0, u1) = self.knots(degrees[0], counts[0])?;
        let (controls_v, v0, v1) = self.knots(degrees[1], counts[1])?;
        self.reals(controls_u.checked_mul(controls_v)?.checked_mul(3 + rational)?)?;
        Some(Net { range: [u0, u1, v0, v1], closure })
    }

    fn surface(&mut self) -> Option<()> {
        match self.id()? {
            "null_surface" => return Some(()),
            "spline" => {
                self.logical(Logical::Sense)?;
                self.block()?;
            }
            "plane" => {
                self.position()?;
                self.direction()?;
                self.direction()?;
                self.logical(Logical::SenseV)?;
            }
            "cone" => {
                self.position()?;
                self.direction()?;
                self.direction()?;
                self.real()?;
                self.interval()?;
                self.reals(3)?;
                self.logical(Logical::Sense)?;
            }
            "sphere" => {
                self.position()?;
                self.real()?;
                self.direction()?;
                self.direction()?;
                self.logical(Logical::SenseV)?;
            }
            "torus" => {
                self.position()?;
                self.direction()?;
                self.reals(2)?;
                self.direction()?;
                self.logical(Logical::SenseV)?;
            }
            _ => return None,
        }
        self.interval()?;
        self.interval()
    }

    fn curve(&mut self) -> Option<()> {
        match self.id()? {
            "null_curve" => return Some(()),
            "intcurve" => {
                self.logical(Logical::Sense)?;
                self.block()?;
            }
            "straight" => {
                self.position()?;
                self.direction()?;
            }
            "ellipse" => {
                self.position()?;
                self.direction()?;
                self.direction()?;
                self.real()?;
            }
            _ => return None,
        }
        self.interval()
    }

    /// The version number and the `full` approximation level ShapeManager adds.
    fn asm_level(&mut self, version: bool) -> Option<()> {
        if self.asm {
            if version {
                self.take_int()?;
            }
            (self.take_enum()? == 0).then_some(())?;
        }
        Some(())
    }

    /// A blend support: its surface, with no curve data.
    fn support(&mut self) -> Option<()> {
        if self.asm {
            self.expect("blend_support_surface")?;
        } else {
            // A 4.0 string is its length and its text (one token in SAB).
            if self.peek() != Some("blendsupsur") {
                (self.take_int()? == 11).then_some(())?;
            }
            self.expect("blendsupsur")?;
        }
        self.out.push(Atom::Str("blendsupsur"));
        self.surface()?;
        self.curve()?;
        self.bs_curve(2, true)?;
        self.position()?;
        if self.asm {
            self.expect("nullbs")?;
            (self.take_int()? == 0).then_some(())?;
            self.expect("nullbs")?;
        }
        Some(())
    }

    /// A `{ subtype ... }` block, written with its 4.0 name and fields.
    fn block(&mut self) -> Option<()> {
        let open = self.at;
        (self.word()? == "{").then_some(())?;
        let name = self.word()?;
        self.out.push(Atom::Open((name != "ref").then_some((self.record, open))));
        let legacy = match (name, self.asm) {
            ("ref", _) => "ref",
            ("exactsur", false) | ("exact_spl_sur", true) => "exactsur",
            // No 4.0 form: AutoCAD refits them; here, their own approximation.
            ("loft_spl_sur" | "sweep_sur", true) => "exactsur",
            ("offset_int_cur", true) => "exactcur",
            ("rotsur", false) | ("rot_spl_sur", true) => "rotsur",
            ("rbblnsur", false) | ("rb_blend_spl_sur", true) => "rbblnsur",
            ("exppc", false) | ("exp_par_cur", true) => "exppc",
            ("exactcur", false) | ("exact_int_cur", true) => "exactcur",
            ("parcur", false) | ("par_int_cur", true) => "parcur",
            ("surfintcur", false) | ("int_int_cur", true) => "surfintcur",
            ("bldcur", false) | ("blend_int_cur", true) => "bldcur",
            ("offintcur", false) | ("off_int_cur", true) => "offintcur",
            _ => return None,
        };
        self.out.push(Atom::Id(legacy.to_string()));
        match legacy {
            "ref" => {
                let number = self.take_int()?;
                self.out.push(Atom::Ref(number));
            }
            "exactsur" if name != "exactsur" && name != "exact_spl_sur" => self.approximation()?,
            "rotsur" => {
                // The profile curve and the axis (root, direction); 4.0 then
                // has the parameter ranges, ShapeManager its approximation.
                if self.asm {
                    self.take_int()?;
                }
                self.curve()?;
                self.position()?;
                self.direction()?;
                if self.asm {
                    (self.take_enum()? == 0).then_some(())?;
                    let mark = self.out.len();
                    let net = self.bs_surface()?;
                    self.take_real()?;
                    self.out.truncate(mark);
                    self.finite(net.range[0], net.range[1]);
                    self.finite(net.range[2], net.range[3]);
                    self.discontinuities(6)?;
                    self.take_logical()?;
                } else {
                    self.interval()?;
                    self.interval()?;
                    self.discontinuities(6)?;
                }
            }
            "exactsur" => {
                self.asm_level(true)?;
                self.bs_surface()?;
                self.real()?;
                self.discontinuities(6)?;
                if self.asm {
                    self.take_logical()?;
                }
                self.interval()?;
                self.interval()?;
                if self.asm {
                    self.take_enum()?;
                }
            }
            "rbblnsur" => self.rolling_ball_blend()?,
            "exppc" => {
                self.bs_curve(2, false)?;
                self.real()?;
                self.surface()?;
            }
            _ => {
                // Interpolated curves: the spline, its two surfaces and their
                // parameter curves, the safe range and the discontinuities.
                self.asm_level(true)?;
                self.bs_curve(3, false)?;
                let fit = self.take_real()?;
                // An approximation that stands as the exact curve fits it exactly.
                self.out.push(Atom::Real(if name == "offset_int_cur" { 0.0 } else { fit }));
                self.surface()?;
                self.surface()?;
                self.bs_curve(2, true)?;
                self.bs_curve(2, true)?;
                self.interval()?;
                self.discontinuities(3)?;
                if name == "offset_int_cur" {
                    // The rest (the curve it offsets and its laws) has no 4.0
                    // form: the approximation above stands as an exact curve,
                    // with the unset range of every exact curve of AutoCAD.
                    self.at = self.block_end()?;
                    self.finite(1.0, 0.0);
                    return (self.id()? == "}").then_some(());
                }
                if self.asm {
                    self.take_int()?;
                }
                match legacy {
                    "exactcur" => {
                        self.interval()?;
                        if self.asm {
                            self.take_enum()?;
                            self.take_enum()?;
                        }
                    }
                    "parcur" => {
                        self.logical(Logical::Surface)?;
                        // ShapeManager adds a second logical, unknown to 4.0.
                        if self.asm {
                            self.take_logical()?;
                        }
                    }
                    "bldcur" => {
                        self.logical(Logical::Surface)?;
                    }
                    "offintcur" => self.reals(2)?,
                    _ => {}
                }
            }
        }
        (self.id()? == "}").then_some(())
    }

    /// The `}` that closes the block being read.
    fn block_end(&self) -> Option<usize> {
        let mut depth = 0usize;
        for (index, token) in self.tokens.iter().enumerate().skip(self.at) {
            match token.as_ident() {
                Some("{") => depth += 1,
                Some("}") if depth == 0 => return Some(index),
                Some("}") => depth -= 1,
                _ => {}
            }
        }
        None
    }

    /// A ShapeManager surface with no 4.0 form (a loft, a sweep), as the exact
    /// spline that closes its block: its own approximation, within its fit
    /// tolerance. Its nested definitions (a sweep's profile) are dropped, and
    /// `document_to_v400` renumbers the `ref`s of the body.
    fn approximation(&mut self) -> Option<()> {
        let end = self.block_end()?;
        let mark = self.out.len();
        for start in (self.at + 1..end).rev() {
            if !matches!(self.tokens[start].as_ident(), Some("nubs" | "nurbs")) {
                continue;
            }
            self.at = start - 1;
            let parsed = (|| {
                (self.take_enum()? == 0).then_some(())?;
                self.bs_surface()?;
                self.take_real()?;
                self.out.push(Atom::Real(0.0));
                self.discontinuities(6)?;
                self.take_logical()?;
                (self.at == end).then_some(())
            })();
            if parsed.is_some() {
                // The unset ranges every exact surface of AutoCAD carries.
                self.finite(1.0, 0.0);
                self.finite(1.0, 0.0);
                return Some(());
            }
            self.out.truncate(mark);
        }
        None
    }

    /// A rolling-ball blend. 4.0 stores no approximation, only its ranges and
    /// closures; ShapeManager stores the whole spline after the tolerances.
    fn rolling_ball_blend(&mut self) -> Option<()> {
        if self.asm {
            self.take_int()?;
        }
        self.support()?;
        self.support()?;
        self.curve()?;
        self.reals(2)?;
        (self.enumeration(Table::Radius)? == -1).then_some(())?;
        self.interval()?;
        if !self.asm {
            self.interval()?;
            self.interval()?;
            self.int()?;
            self.int()?;
        }
        let mark = self.out.len();
        self.interval()?;
        self.int()?;
        self.reals(2)?;
        self.int()?;
        if self.asm {
            let tail = self.out.split_off(mark);
            (self.take_enum()? == 0).then_some(())?;
            let net = self.bs_surface()?;
            self.real()?;
            self.discontinuities(6)?;
            self.take_logical()?;
            for _ in 0..3 {
                self.take_int()?;
            }
            self.out.truncate(mark);
            self.finite(net.range[0], net.range[1]);
            self.finite(net.range[2], net.range[3]);
            self.out.extend(net.closure.map(Atom::Int));
            self.out.extend(tail);
        }
        Some(())
    }

    fn record(&mut self, entity_type: &str) -> Option<()> {
        // The leading `$-1` of the 7.0 layout (a sentinel in a 4.0 body).
        let first = self.tokens.first()?;
        first.as_pointer()?;
        self.out.push(Atom::Raw(first.clone()));
        self.at = 1;
        match entity_type {
            "spline-surface" => {
                self.logical(Logical::Sense)?;
                self.block()?;
                self.interval()?;
                self.interval()?;
            }
            "intcurve-curve" => {
                self.logical(Logical::Sense)?;
                self.block()?;
                self.interval()?;
            }
            "pcurve" => {
                if self.int()? == 0 {
                    self.logical(Logical::Sense)?;
                    self.block()?;
                } else {
                    let pointer = self.tokens.get(self.at)?;
                    pointer.as_pointer()?;
                    self.out.push(Atom::Raw(pointer.clone()));
                    self.at += 1;
                }
                self.reals(2)?;
            }
            _ => return None,
        }
        (self.at == self.tokens.len()).then_some(())
    }
}

fn sab(tag: u8, data: Vec<u8>) -> SatToken {
    SatToken::Sab { tag, data }
}

fn counted(tag: u8, text: &str) -> SatToken {
    let mut data = vec![text.len() as u8];
    data.extend_from_slice(text.as_bytes());
    sab(tag, data)
}

fn triplet(tag: u8, vector: [f64; 3]) -> SatToken {
    sab(tag, vector.iter().flat_map(|v| v.to_le_bytes()).collect())
}

/// The atoms as binary tokens, each with the tag ShapeManager gives its kind.
fn binary(atoms: Vec<Atom>) -> Vec<SatToken> {
    atoms
        .into_iter()
        .map(|atom| match atom {
            Atom::Raw(token) => token,
            Atom::Open(_) => sab(0x0F, Vec::new()),
            Atom::Int(value) | Atom::Ref(value) => sab(0x04, (value as i32).to_le_bytes().to_vec()),
            Atom::Real(value) => sab(0x06, value.to_le_bytes().to_vec()),
            Atom::Logical(value, _) => sab(if value { 0x0B } else { 0x0A }, Vec::new()),
            Atom::Enum(value, _) => sab(0x15, (value as i32).to_le_bytes().to_vec()),
            Atom::Id(name) => match name.as_str() {
                "{" => sab(0x0F, Vec::new()),
                "}" => sab(0x10, Vec::new()),
                _ => counted(0x0D, &name),
            },
            Atom::Str(text) => counted(0x07, text),
            Atom::Position(vector) => triplet(0x13, vector),
            Atom::Direction(vector) => triplet(0x14, vector),
        })
        .collect()
}

/// The atoms as SAT text tokens, with the keywords of the 4.0 text.
fn text(atoms: Vec<Atom>) -> Option<Vec<SatToken>> {
    let id = |name: &str| SatToken::Ident(name.to_string());
    let mut tokens = Vec::with_capacity(atoms.len());
    for atom in atoms {
        match atom {
            Atom::Raw(token) => tokens.push(token),
            Atom::Open(_) => tokens.push(id("{")),
            Atom::Int(value) | Atom::Ref(value) => tokens.push(SatToken::Integer(value)),
            Atom::Real(value) => tokens.push(SatToken::Float(value)),
            Atom::Logical(value, kind) => tokens.push(id(match (kind, value) {
                (Logical::Bound, true) => "I",
                (Logical::Bound, false) => "F",
                (Logical::Sense, true) => "forward",
                (Logical::Sense, false) => "reversed",
                (Logical::SenseV, true) => "forward_v",
                (Logical::SenseV, false) => "reverse_v",
                (Logical::Surface, true) => "surf2",
                (Logical::Surface, false) => "surf1",
            })),
            Atom::Enum(value, table) => tokens.push(id(match (table, value) {
                (Table::Closure, 0) => "open",
                (Table::Closure, 1) => "closed",
                (Table::Closure, 2) => "periodic",
                (Table::Singularity, 0) => "none",
                (Table::Singularity, 1) => "low",
                (Table::Singularity, 2) => "high",
                (Table::Singularity, 3) => "both",
                (Table::Radius, -1) => "no_radius",
                _ => return None,
            })),
            Atom::Id(name) => tokens.push(SatToken::Ident(name)),
            Atom::Str(value) => tokens.push(SatToken::String(value.to_string())),
            Atom::Position(vector) | Atom::Direction(vector) => {
                tokens.extend(vector.map(SatToken::Float))
            }
        }
    }
    Some(tokens)
}

/// Whether the record carries a spline subtype block.
pub(crate) fn is_spline(record: &SatRecord) -> bool {
    matches!(record.entity_type.as_str(), "spline-surface" | "intcurve-curve" | "pcurve")
}

/// The tokens of a spline record in the ACIS 4.0 dialect: read from the
/// ShapeManager one (`asm`) or from 4.0 itself, and written for SAB
/// (`as_binary`) or for SAT text. `None` when a subtype is not known.
pub(crate) fn to_v400(record: &SatRecord, asm: bool, as_binary: bool) -> Option<Vec<SatToken>> {
    let mut walker = Walker { tokens: &record.tokens, record: 0, at: 0, asm, out: Vec::new() };
    walker.record(&record.entity_type)?;
    if as_binary {
        Some(binary(walker.out))
    } else {
        text(walker.out)
    }
}

/// The spline records of a ShapeManager body as 4.0 SAT text (`None` for the
/// other records and for those that cannot cross). A `ref n` numbers the
/// subtype definitions of the body in the order they open, nested ones
/// included; the approximated subtypes drop theirs (a sweep's profile), so the
/// `ref`s are renumbered, and one to a dropped definition gets that
/// definition written in its place, as AutoCAD does.
pub(crate) fn document_to_v400(records: &[SatRecord]) -> Vec<Option<Vec<SatToken>>> {
    // The source numbering: every `{` that does not open a `ref`.
    let mut definitions = Vec::new();
    for (index, record) in records.iter().enumerate() {
        let tokens = &record.tokens;
        for at in 0..tokens.len() {
            if tokens[at].as_ident() == Some("{") && tokens.get(at + 1).and_then(|t| t.as_ident()) != Some("ref") {
                definitions.push((index, at));
            }
        }
    }
    let walk = |index: usize, at: usize, block: bool| {
        let record = &records[index];
        let mut walker = Walker { tokens: &record.tokens, record: index, at, asm: true, out: Vec::new() };
        let walked = if block { walker.block() } else { walker.record(&record.entity_type) };
        walked.map(|_| walker.out)
    };
    let mut renumbered = std::collections::HashMap::new();
    let mut next = 0i64;
    let mut out = Vec::with_capacity(records.len());
    for (index, record) in records.iter().enumerate() {
        let start = next;
        let walked = if is_spline(record) { walk(index, 0, false) } else { None };
        let atoms = walked.and_then(|mut atoms| {
            let mut at = 0;
            while at < atoms.len() {
                match atoms[at] {
                    Atom::Open(Some(source)) => {
                        renumbered.insert(source, next);
                        next += 1;
                    }
                    Atom::Ref(number) => {
                        let source = *definitions.get(usize::try_from(number).ok()?)?;
                        if let Some(&new) = renumbered.get(&source) {
                            atoms[at] = Atom::Ref(new);
                        } else {
                            // `{ ref n }` becomes the definition itself, read again.
                            let inline = walk(source.0, source.1, true)?;
                            atoms.splice(at.checked_sub(2)?..at + 2, inline);
                            at -= 2;
                            continue;
                        }
                    }
                    _ => {}
                }
                at += 1;
            }
            text(atoms)
        });
        if atoms.is_none() {
            // Not translated: its definitions keep their place.
            renumbered.retain(|_, new| *new < start);
            next = start;
            for &(_, at) in definitions.iter().filter(|(r, _)| *r == index) {
                renumbered.insert((index, at), next);
                next += 1;
            }
        }
        out.push(atoms);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::{SabReader, SabWriter, SatDocument};

    const HEADER_400: &str = "400 0 1 0\n16 Autodesk AutoCAD 17 ASM 227.0.0.65535 NT 0\n1 9.9999999999999995e-07 1e-10\n";
    const HEADER_ASM: &str =
        "22300 0 1 0\n@16 Autodesk AutoCAD @19 ASM 227.0.0.65535 NT @0 \n1 9.9999999999999995e-07 1e-10\n";

    // Records of the NURBS surface of `tools/solidos`, as AutoCAD 2027 writes
    // them in DWG 2000 and (the same ones) in DWG 2018.
    const CURVE_400: &str = "intcurve-curve $-1 forward { exactcur nubs 1 open 2 0 1 10 1 120 80 0 130 80 0 0 null_surface null_surface nullbs nullbs I I 0 0 0 F 1 F 0 } I I #";
    const CURVE_ASM: &str = "intcurve-curve $-1 -1 $-1 forward { exact_int_cur 20900 full nubs 1 open 2 0 1 10 1 120 80 0 130 80 0 0 null_surface null_surface nullbs nullbs I I 0 0 0 0 F 1 F 0 UNEXTENDED UNEXTENDED } I I #";
    const PCURVE_400: &str = "pcurve $-1 0 forward { exppc nubs 1 open 2 0 1 10 1 0 0 1 0 0 spline forward { ref 0 } I I I I } 0 0 #";
    const PCURVE_ASM: &str = "pcurve $-1 -1 $-1 0 forward { exp_par_cur nubs 1 open 2 0 1 10 1 0 0 1 0 0 spline forward { ref 0 } I I I I } 0 0 #";

    /// ShapeManager subtypes come out with their 4.0 names and fields.
    #[test]
    fn shapemanager_splines_become_v400_text() {
        let asm = SatDocument::parse(&format!("{HEADER_ASM}{CURVE_ASM}\n{PCURVE_ASM}\nEnd-of-ASM-data\n")).unwrap();
        assert!(asm.splines_cross_schema());
        let legacy = asm.to_legacy_v400().to_sat_string();
        assert!(legacy.contains(CURVE_400), "{legacy}");
        assert!(legacy.contains(PCURVE_400), "{legacy}");
    }

    /// A 4.0 body goes into SAB with each field under its own tag: whole
    /// numbers that are knots or coordinates as doubles, counts as integers.
    #[test]
    fn v400_splines_are_tagged_for_sab() {
        let legacy = SatDocument::parse(&format!("{HEADER_400}{CURVE_400}\n{PCURVE_400}\n")).unwrap();
        assert!(legacy.splines_cross_schema());
        let sab = SabWriter::write(&legacy);
        // `{ exactcur nubs 1 open 2 0. 1 10. 1 120. ...`
        let expected: Vec<u8> = [
            &[0x0F, 0x0D, 8][..],
            b"exactcur",
            &[0x0D, 4],
            b"nubs",
            &[0x04, 1, 0, 0, 0, 0x15, 0, 0, 0, 0, 0x04, 2, 0, 0, 0, 0x06],
            &0f64.to_le_bytes(),
            &[0x04, 1, 0, 0, 0, 0x06],
            &10f64.to_le_bytes(),
            &[0x04, 1, 0, 0, 0, 0x06],
            &120f64.to_le_bytes(),
        ]
        .concat();
        assert!(sab.windows(expected.len()).any(|window| window == expected));
        let back = SabReader::read(&sab).unwrap();
        assert_eq!(back.records.len(), 2);
        // Read back, the records are the 4.0 text again.
        let text = back.to_sat_string();
        assert!(text.contains(CURVE_400) && text.contains(PCURVE_400), "{text}");
    }

    // SOL-04: a surface of revolution (REVOLVE of a spline), with the same
    // fields as the ones AutoCAD 2027 writes in DWG 2000 and 2018: 4.0 keeps
    // the parameter ranges, ShapeManager its approximation (whose knots span
    // the same ranges) and the same discontinuities.
    const EXACT_ASM: &str = "intcurve forward { exact_int_cur 20900 full nubs 1 open 2 0 1 10 1 120 80 0 130 80 0 0 null_surface null_surface nullbs nullbs I I 0 0 0 0 F 1 F 0 UNEXTENDED UNEXTENDED } I I";
    const EXACT_400: &str = "intcurve forward { exactcur nubs 1 open 2 0 1 10 1 120 80 0 130 80 0 0 null_surface null_surface nullbs nullbs I I 0 0 0 F 1 F 0 } I I";
    const NET: &str = "nubs 1 1 open open none none 2 2 0 1 10 1 0 1 6.25 1 0 0 0 1 0 0 0 1 0 1 1 0";

    #[test]
    fn revolved_spline_crosses_both_ways() {
        let asm = format!("spline-surface $-1 -1 $-1 forward {{ rot_spl_sur 23100 {EXACT_ASM} 0 0 0 0 1 0 full {NET} 1e-06 0 0 2 0 10 0 0 0 I }} I I I I #");
        let rot_400 = format!("spline-surface $-1 forward {{ rotsur {EXACT_400} 0 0 0 0 1 0 F 0 F 10 F 0 F 6.25 0 0 2 0 10 0 0 0 }} I I I I #");
        let doc = SatDocument::parse(&format!("{HEADER_ASM}{asm}\nEnd-of-ASM-data\n")).unwrap();
        assert!(doc.splines_cross_schema());
        let legacy = doc.to_legacy_v400().to_sat_string();
        assert!(legacy.contains(&rot_400), "{legacy}");
        // And the 4.0 body into the SAB of a 2013+ drawing, and back.
        let legacy = SatDocument::parse(&format!("{HEADER_400}{rot_400}\n")).unwrap();
        assert!(legacy.splines_cross_schema());
        let back = SabReader::read(&SabWriter::write(&legacy)).unwrap().to_sat_string();
        assert!(back.contains(&rot_400), "{back}");
    }

    /// A sweep has no 4.0 form: it becomes its approximation, its profile
    /// definition is dropped, and the `ref`s after it are renumbered; one to
    /// the profile gets the profile written in its place.
    #[test]
    fn sweep_becomes_its_approximation_and_refs_renumber() {
        let body = [
            // 0 (sweep), 1 (its profile)
            format!("spline-surface $-1 -1 $-1 forward {{ sweep_sur 23100 F -2 {EXACT_ASM} F 0 F 6 0 full {NET} 0.0003 0 0 0 0 0 0 I }} I I I I #"),
            // 2
            CURVE_ASM.to_string(),
            "intcurve-curve $-1 -1 $-1 forward { ref 1 } I I #".to_string(),
            "intcurve-curve $-1 -1 $-1 forward { ref 2 } I I #".to_string(),
            "intcurve-curve $-1 -1 $-1 reversed { ref 1 } I I #".to_string(),
            "pcurve $-1 -1 $-1 0 forward { exp_par_cur nubs 1 open 2 0 1 10 1 0 0 1 0 0 spline forward { ref 0 } I I I I } 0 0 #".to_string(),
        ];
        let doc = SatDocument::parse(&format!("{HEADER_ASM}{}\nEnd-of-ASM-data\n", body.join("\n"))).unwrap();
        assert!(doc.splines_cross_schema());
        let legacy = doc.to_legacy_v400().to_sat_string();
        let lines: Vec<&str> = legacy.lines().filter(|l| l.contains('{')).collect();
        assert!(lines[0].contains("{ exactsur nubs 1 1 open open none none 2 2") && !legacy.contains("sweep"), "{legacy}");
        // 1: the curve of record 2; 2: the profile, written where it is used first.
        assert!(lines[1].contains(CURVE_400), "{legacy}");
        assert!(lines[2].contains("{ exactcur nubs 1 open 2 0 1 10 1 120 80 0"), "{legacy}");
        assert!(lines[3].contains("{ ref 1 }") && lines[4].contains("{ ref 2 }"), "{legacy}");
        assert!(lines[5].contains("spline forward { ref 0 }"), "{legacy}");
    }

    /// The offset edge of a tapered extrusion: its approximation, as an exact
    /// curve; the curve it offsets is dropped and the `ref`s renumbered.
    #[test]
    fn offset_curve_becomes_its_approximation() {
        let offset = format!("intcurve-curve $-1 -1 $-1 forward {{ offset_int_cur 23100 full nubs 1 open 2 0 1 10 1 120 80 0 130 80 0 0.0002 null_surface null_surface nullbs nullbs I I 0 0 0 0 {EXACT_ASM} 0 6 0 0 1 }} I I #");
        let body = [offset.as_str(), CURVE_ASM, "intcurve-curve $-1 -1 $-1 forward { ref 2 } I I #"].join("\n");
        let doc = SatDocument::parse(&format!("{HEADER_ASM}{body}\nEnd-of-ASM-data\n")).unwrap();
        assert!(doc.splines_cross_schema());
        let legacy = doc.to_legacy_v400().to_sat_string();
        let lines: Vec<&str> = legacy.lines().filter(|l| l.contains('{')).collect();
        assert!(lines[0].contains(CURVE_400) && lines[1].contains(CURVE_400), "{legacy}");
        assert!(lines[2].contains("{ ref 1 }"), "{legacy}");
    }

    /// A subtype with no known translation is reported, not guessed.
    #[test]
    fn unknown_spline_subtype_does_not_cross() {
        let sweep = CURVE_ASM.replace("exact_int_cur", "law_int_cur");
        let reference = "intcurve-curve $-1 -1 $-1 forward { ref 0 } I I #";
        let asm = SatDocument::parse(&format!("{HEADER_ASM}{sweep}\n{reference}\nEnd-of-ASM-data\n")).unwrap();
        assert!(!asm.splines_cross_schema());
        // It stays as it is, and keeps its number.
        let legacy = asm.to_legacy_v400().to_sat_string();
        assert!(legacy.contains("law_int_cur") && legacy.contains("{ ref 0 }"), "{legacy}");
    }
}

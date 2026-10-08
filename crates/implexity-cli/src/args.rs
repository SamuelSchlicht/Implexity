// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;
use std::fmt::Write as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Flag,
    Str,
    Int,
    Float,
}

#[derive(Clone, Debug)]
pub(crate) struct Opt {
    pub(crate) long: &'static str,
    pub(crate) short: Option<&'static str>,
    pub(crate) kind: Kind,
    pub(crate) nargs: usize,
    pub(crate) metavar: &'static [&'static str],
    pub(crate) help: &'static str,
    pub(crate) append: bool,
    pub(crate) choices: &'static [&'static str],
    pub(crate) required: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PosN {
    One,
    Optional,
    Any,
    Some,
}

#[derive(Clone, Debug)]
pub(crate) struct Positional {
    pub(crate) name: &'static str,
    pub(crate) nargs: PosN,
    pub(crate) help: &'static str,
    pub(crate) choices: &'static [&'static str],
}

#[derive(Clone, Debug)]
pub(crate) struct Parser {
    pub(crate) prog: String,
    pub(crate) description: String,
    pub(crate) epilog: String,
    pub(crate) opts: Vec<Opt>,
    pub(crate) positionals: Vec<Positional>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Val {
    Flag,
    Strs(Vec<String>),
    Ints(Vec<i64>),
    Floats(Vec<f64>),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Parsed {
    values: BTreeMap<&'static str, Val>,
    positionals: BTreeMap<&'static str, Vec<String>>,
}

impl Parsed {
    #[must_use]
    pub(crate) fn flag(&self, long: &str) -> bool {
        matches!(self.values.get(long), Some(Val::Flag))
    }

    #[must_use]
    pub(crate) fn str(&self, long: &str) -> Option<&str> {
        match self.values.get(long) {
            Some(Val::Strs(v)) => v.last().map(String::as_str),
            _ => None,
        }
    }

    #[must_use]
    pub(crate) fn strs(&self, long: &str) -> &[String] {
        match self.values.get(long) {
            Some(Val::Strs(v)) => v,
            _ => &[],
        }
    }

    #[must_use]
    pub(crate) fn ints(&self, long: &str) -> Option<&[i64]> {
        match self.values.get(long) {
            Some(Val::Ints(v)) => Some(v),
            _ => None,
        }
    }

    #[must_use]
    pub(crate) fn floats(&self, long: &str) -> Option<&[f64]> {
        match self.values.get(long) {
            Some(Val::Floats(v)) => Some(v),
            _ => None,
        }
    }

    #[must_use]
    pub(crate) fn int(&self, long: &str) -> Option<i64> {
        self.ints(long).and_then(|v| v.first().copied())
    }

    #[must_use]
    pub(crate) fn float(&self, long: &str) -> Option<f64> {
        self.floats(long).and_then(|v| v.first().copied())
    }

    #[must_use]
    pub(crate) fn pos(&self, name: &str) -> &[String] {
        self.positionals.get(name).map_or(&[], Vec::as_slice)
    }

    #[must_use]
    pub(crate) fn pos1(&self, name: &str) -> Option<&str> {
        self.pos(name).first().map(String::as_str)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Exit {
    Help(String),
    Error(String),
}



pub(crate) fn py_int(text: &str) -> Result<i64, ()> {
    let t = text.trim();
    let digits = t.trim_start_matches(['+', '-']);
    let ok_underscores = !digits.starts_with('_') && !digits.ends_with('_') && !digits.contains("__");
    if !ok_underscores || digits.is_empty() || t.len() - digits.len() > 1 {
        return Err(());
    }
    t.replace('_', "").parse().map_err(|_| ())
}



pub(crate) fn py_float(text: &str) -> Result<f64, ()> {
    let t = text.trim();
    let lower = t.to_ascii_lowercase();
    let body = lower.trim_start_matches(['+', '-']);
    if t.len() - body.len() > 1 {
        return Err(());
    }
    let sign = if lower.starts_with('-') { -1.0 } else { 1.0 };
    match body {
        "inf" | "infinity" => return Ok(sign * f64::INFINITY),
        "nan" => return Ok(f64::NAN),
        _ => {}
    }
    if body.starts_with('_')
        || body.ends_with('_')
        || body.contains("__")
        || body.contains("_.")
        || body.contains("._")
    {
        return Err(());
    }
    if !body.bytes().all(|b| b.is_ascii_digit() || b".e+-_".contains(&b)) {
        return Err(());
    }
    t.replace('_', "").parse().map_err(|_| ())
}

impl Parser {
    #[must_use]
    pub(crate) fn new(prog: &str, description: &str) -> Self {
        Self {
            prog: prog.to_owned(),
            description: description.to_owned(),
            epilog: String::new(),
            opts: Vec::new(),
            positionals: Vec::new(),
        }
    }

    #[must_use]
    pub(crate) fn epilog(mut self, text: &str) -> Self {
        text.clone_into(&mut self.epilog);
        self
    }

    #[must_use]
    pub(crate) fn opt(mut self, long: &'static str, kind: Kind, nargs: usize, help: &'static str) -> Self {
        self.opts.push(Opt {
            long,
            short: None,
            kind,
            nargs: if kind == Kind::Flag { 0 } else { nargs },
            metavar: &[],
            help,
            append: false,
            choices: &[],
            required: false,
        });
        self
    }

    #[must_use]
    pub(crate) fn opt_meta(
        mut self,
        long: &'static str,
        kind: Kind,
        metavar: &'static [&'static str],
        help: &'static str,
    ) -> Self {
        self.opts.push(Opt {
            long,
            short: None,
            kind,
            nargs: metavar.len(),
            metavar,
            help,
            append: false,
            choices: &[],
            required: false,
        });
        self
    }

    fn last(&mut self) -> Option<&mut Opt> {
        self.opts.last_mut()
    }

    #[must_use]
    pub(crate) fn short(mut self, short: &'static str) -> Self {
        if let Some(o) = self.last() {
            o.short = Some(short);
        }
        self
    }

    #[must_use]
    pub(crate) fn append(mut self) -> Self {
        if let Some(o) = self.last() {
            o.append = true;
        }
        self
    }

    #[must_use]
    pub(crate) fn choices(mut self, choices: &'static [&'static str]) -> Self {
        if let Some(o) = self.last() {
            o.choices = choices;
        }
        self
    }

    #[must_use]
    pub(crate) fn required(mut self) -> Self {
        if let Some(o) = self.last() {
            o.required = true;
        }
        self
    }

    #[must_use]
    pub(crate) fn metavar(mut self, metavar: &'static [&'static str]) -> Self {
        if let Some(o) = self.last() {
            o.metavar = metavar;
        }
        self
    }

    #[must_use]
    pub(crate) fn pos(mut self, name: &'static str, nargs: PosN, help: &'static str) -> Self {
        self.positionals.push(Positional { name, nargs, help, choices: &[] });
        self
    }

    fn metavars(o: &Opt) -> Vec<String> {
        if !o.metavar.is_empty() {
            if o.metavar.len() == o.nargs {
                return o.metavar.iter().map(|s| (*s).to_owned()).collect();
            }
            return vec![o.metavar[0].to_owned(); o.nargs];
        }
        if !o.choices.is_empty() {
            return vec![format!("{{{}}}", o.choices.join(",")); o.nargs];
        }
        let m = o.long.trim_start_matches('-').replace('-', "_").to_uppercase();
        vec![m; o.nargs]
    }

    fn pos_usage(p: &Positional) -> String {
        let n = if p.choices.is_empty() { p.name.to_owned() } else { format!("{{{}}}", p.choices.join(",")) };
        match p.nargs {
            PosN::One => n,
            PosN::Optional => format!("[{n}]"),
            PosN::Any => format!("[{n} ...]"),
            PosN::Some => format!("{n} [{n} ...]"),
        }
    }

    fn opt_name(o: &Opt) -> String {
        o.short.map_or_else(|| o.long.to_owned(), |s| format!("{s}/{}", o.long))
    }

    #[must_use]
    pub(crate) fn usage(&self) -> String {
        let mut u = format!("usage: {} [-h]", self.prog);
        for o in &self.opts {
            let mv = Self::metavars(o);
            let name = o.short.unwrap_or(o.long);
            let body = if mv.is_empty() { name.to_owned() } else { format!("{name} {}", mv.join(" ")) };
            if o.required {
                let _ = write!(u, " {body}");
            } else {
                let _ = write!(u, " [{body}]");
            }
        }
        for p in &self.positionals {
            let _ = write!(u, " {}", Self::pos_usage(p));
        }
        u
    }

    #[must_use]
    pub(crate) fn help(&self) -> String {
        let mut h = self.usage();
        h.push_str("\n\n");
        if !self.description.is_empty() {
            h.push_str(self.description.trim_end());
            h.push_str("\n\n");
        }
        if !self.positionals.is_empty() {
            h.push_str("positional arguments:\n");
            for p in &self.positionals {
                let head = if p.choices.is_empty() {
                    p.name.to_owned()
                } else {
                    format!("{{{}}}", p.choices.join(","))
                };
                if head.len() > 22 {
                    let _ = writeln!(h, "  {head}\n  {:<22} {}", "", p.help);
                } else {
                    let _ = writeln!(h, "  {head:<22} {}", p.help);
                }
            }
            h.push('\n');
        }
        h.push_str("options:\n  -h, --help             show this help message and exit\n");
        for o in &self.opts {
            let mv = Self::metavars(o);
            let names = match o.short {
                Some(s) if mv.is_empty() => format!("{s}, {}", o.long),
                Some(s) => format!("{s} {m}, {} {m}", o.long, m = mv.join(" ")),
                None if mv.is_empty() => o.long.to_owned(),
                None => format!("{} {}", o.long, mv.join(" ")),
            };
            if names.len() > 22 {
                let _ = writeln!(h, "  {names}\n  {:<22} {}", "", o.help);
            } else {
                let _ = writeln!(h, "  {names:<22} {}", o.help);
            }
        }
        if !self.epilog.is_empty() {
            h.push('\n');
            h.push_str(self.epilog.trim_end());
            h.push('\n');
        }
        h
    }

    #[must_use]
    pub(crate) fn error(&self, message: &str) -> Exit {
        Exit::Error(format!("{}\n{}: error: {message}\n", self.usage(), self.prog))
    }

    fn resolve(&self, given: &str) -> Result<&Opt, Exit> {
        if let Some(o) = self.opts.iter().find(|o| o.long == given) {
            return Ok(o);
        }
        let hits: Vec<&Opt> = self.opts.iter().filter(|o| o.long.starts_with(given)).collect();
        let help_hit = "--help".starts_with(given) && given.len() > 2;
        match (hits.as_slice(), help_hit) {
            ([one], false) => Ok(one),
            ([], true) => Err(Exit::Help(self.help())),
            ([], false) => Err(self.error(&format!("unrecognized arguments: {given}"))),
            (many, _) => {
                let mut names: Vec<&str> = many.iter().map(|o| o.long).collect();
                if help_hit {
                    names.insert(0, "--help");
                }
                Err(self.error(&format!("ambiguous option: {given} could match {}", names.join(", "))))
            }
        }
    }

    fn convert(&self, o: &Opt, raw: &[String]) -> Result<Val, Exit> {
        let shown = Self::opt_name(o);
        let bad = |kind: &str, v: &str| self.error(&format!("argument {shown}: invalid {kind} value: '{v}'"));
        if !o.choices.is_empty()
            && let Some(v) = raw.iter().find(|v| !o.choices.contains(&v.as_str()))
        {
            let options: Vec<String> = o.choices.iter().map(|c| format!("'{c}'")).collect();
            return Err(self.error(&format!(
                "argument {shown}: invalid choice: '{v}' (choose from {})",
                options.join(", ")
            )));
        }
        Ok(match o.kind {
            Kind::Flag => Val::Flag,
            Kind::Str => Val::Strs(raw.to_vec()),
            Kind::Int => Val::Ints(
                raw.iter().map(|v| py_int(v).map_err(|()| bad("int", v))).collect::<Result<_, _>>()?,
            ),
            Kind::Float => Val::Floats(
                raw.iter().map(|v| py_float(v).map_err(|()| bad("float", v))).collect::<Result<_, _>>()?,
            ),
        })
    }

    fn looks_like_option(s: &str) -> bool {
        s.starts_with('-') && s.len() > 1 && py_float(s).is_err()
    }

    fn store(out: &mut Parsed, o: &Opt, v: Val) {
        if o.append
            && let (Some(Val::Strs(prev)), Val::Strs(new)) = (out.values.get_mut(o.long), &v)
        {
            prev.extend(new.iter().cloned());
            return;
        }
        out.values.insert(o.long, v);
    }



    pub(crate) fn parse(&self, argv: &[String]) -> Result<Parsed, Exit> {
        let mut out = Parsed::default();
        let mut positionals: Vec<String> = Vec::new();
        let mut unrecognized: Vec<String> = Vec::new();
        let mut i = 0;
        let mut only_positionals = false;
        while i < argv.len() {
            let a = &argv[i];
            i += 1;
            if only_positionals || !Self::looks_like_option(a) {
                positionals.push(a.clone());
                continue;
            }
            if a == "--" {
                only_positionals = true;
                continue;
            }
            if a == "-h" || a == "--help" {
                return Err(Exit::Help(self.help()));
            }
            let (o, inline): (&Opt, Option<String>) = if a.starts_with("--") {
                let (name, inline) = match a.split_once('=') {
                    Some((n, v)) => (n, Some(v.to_owned())),
                    None => (a.as_str(), None),
                };
                (self.resolve(name)?, inline)
            } else {
                let key = a.get(..2).unwrap_or(a);
                if let Some(o) = self.opts.iter().find(|o| o.short == Some(key)) {
                    (o, a.get(2..).filter(|r| !r.is_empty()).map(str::to_owned))
                } else {
                    unrecognized.push(a.clone());
                    continue;
                }
            };
            let shown = Self::opt_name(o);
            let raw: Vec<String> = if o.nargs == 0 {
                if let Some(v) = inline {
                    return Err(self.error(&format!("argument {shown}: ignored explicit argument '{v}'")));
                }
                Vec::new()
            } else if let Some(v) = inline {
                if o.nargs != 1 {
                    return Err(self.error(&format!("argument {shown}: expected {} arguments", o.nargs)));
                }
                vec![v]
            } else {
                let mut vals = Vec::new();
                while vals.len() < o.nargs && i < argv.len() && !Self::looks_like_option(&argv[i]) {
                    vals.push(argv[i].clone());
                    i += 1;
                }
                if vals.len() < o.nargs {
                    let msg = if o.nargs == 1 {
                        format!("argument {shown}: expected one argument")
                    } else {
                        format!("argument {shown}: expected {} arguments", o.nargs)
                    };
                    return Err(self.error(&msg));
                }
                vals
            };
            let v = self.convert(o, &raw)?;
            Self::store(&mut out, o, v);
        }
        self.assign_positionals(&mut out, positionals, &mut unrecognized)?;
        if !unrecognized.is_empty() {
            return Err(self.error(&format!("unrecognized arguments: {}", unrecognized.join(" "))));
        }
        let missing: Vec<String> = self
            .opts
            .iter()
            .filter(|o| o.required && !out.values.contains_key(o.long))
            .map(Self::opt_name)
            .chain(self.missing_positionals(&out))
            .collect();
        if !missing.is_empty() {
            return Err(self.error(&format!("the following arguments are required: {}", missing.join(", "))));
        }
        Ok(out)
    }

    fn missing_positionals(&self, out: &Parsed) -> Vec<String> {
        self.positionals
            .iter()
            .filter(|p| matches!(p.nargs, PosN::One | PosN::Some) && !out.positionals.contains_key(p.name))
            .map(|p| p.name.to_owned())
            .collect()
    }

    fn assign_positionals(
        &self,
        out: &mut Parsed,
        words: Vec<String>,
        unrecognized: &mut Vec<String>,
    ) -> Result<(), Exit> {
        let mut rest = words.into_iter().collect::<std::collections::VecDeque<_>>();
        for (k, p) in self.positionals.iter().enumerate() {
            let later_min: usize = self.positionals[k + 1..]
                .iter()
                .map(|q| usize::from(matches!(q.nargs, PosN::One | PosN::Some)))
                .sum();
            let available = rest.len().saturating_sub(later_min);
            let take = match p.nargs {
                PosN::One => usize::from(available >= 1),
                PosN::Optional => available.min(1),
                PosN::Any | PosN::Some => available,
            };
            if take == 0 {
                continue;
            }
            let vals: Vec<String> = rest.drain(..take).collect();
            if !p.choices.is_empty()
                && let Some(v) = vals.iter().find(|v| !p.choices.contains(&v.as_str()))
            {
                let options: Vec<String> = p.choices.iter().map(|c| format!("'{c}'")).collect();
                return Err(self.error(&format!(
                    "argument {}: invalid choice: '{v}' (choose from {})",
                    p.name,
                    options.join(", ")
                )));
            }
            out.positionals.insert(p.name, vals);
        }
        unrecognized.extend(rest);
        Ok(())
    }
}


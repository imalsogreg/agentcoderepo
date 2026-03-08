//! AgentCodeRepo universal type system.
//!
//! A language-independent type representation powerful enough to express
//! parametric polymorphism, higher-kinded types, typeclass constraints,
//! structural records/variants, and algebraic effects.
//!
//! Inspired by System F-omega with row types and Unison's ability system.
//!
//! # Canonical text form
//!
//! ```text
//! forall a. Ord a => List a -> List a
//! Text ->{IO, Fail HttpError} Response
//! forall (f : Type -> Type) a b. Functor f => (a -> b) -> f a -> f b
//! { name: String, age: Int }
//! < Ok: a | Err: e >
//! ```

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Core type AST
// ---------------------------------------------------------------------------

/// A AgentCodeRepo universal type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ty {
    /// Primitive / built-in type.
    Prim(Prim),

    /// Type variable introduced by `forall`.
    Var(TyVar),

    /// Function type with an effect set on the arrow.
    /// `param ->{effects} ret`
    Fun {
        param: Box<Ty>,
        effects: EffectSet,
        ret: Box<Ty>,
    },

    /// Type constructor application: `List a`, `Map k v`, `Result e a`.
    App {
        con: Box<Ty>,
        args: Vec<Ty>,
    },

    /// Structural record: `{ name: String, age: Int }`.
    Record(Vec<Field>),

    /// Structural variant (sum): `< Ok: a | Err: e >`.
    Variant(Vec<Field>),

    /// Tuple: `(a, b, c)`. Unit is the empty tuple `()`.
    Tuple(Vec<Ty>),

    /// Universal quantification with optional constraints.
    /// `forall a b. (Ord a, Show b) => body`
    Forall {
        vars: Vec<TyVarBinding>,
        constraints: Vec<Constraint>,
        body: Box<Ty>,
    },

    /// Named / nominal type reference (user-defined types not in scope as a Var).
    Named(String),
}

/// A named field in a record or variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    pub ty: Ty,
}

// ---------------------------------------------------------------------------
// Primitives
// ---------------------------------------------------------------------------

/// Built-in primitive types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Prim {
    Int,
    Float,
    String,
    Bool,
    Bytes,
    Unit,
    Never,
}

// ---------------------------------------------------------------------------
// Type variables & kinds
// ---------------------------------------------------------------------------

/// A type variable name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TyVar(pub String);

/// A type variable binding with an optional kind annotation.
/// In `forall (f : Type -> Type) a.`, `f` has kind `Type -> Type`
/// and `a` has the default kind `Type`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TyVarBinding {
    pub var: TyVar,
    pub kind: Kind,
}

impl TyVarBinding {
    pub fn simple(name: impl Into<String>) -> Self {
        Self {
            var: TyVar(name.into()),
            kind: Kind::Type,
        }
    }

    pub fn higher(name: impl Into<String>, kind: Kind) -> Self {
        Self {
            var: TyVar(name.into()),
            kind,
        }
    }
}

/// The kind of a type. Kinds classify types the way types classify values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// `Type` — the kind of concrete types like `Int`, `String`, `List Int`.
    Type,
    /// `k1 -> k2` — the kind of type constructors.
    /// `List` has kind `Type -> Type`. `Map` has kind `Type -> Type -> Type`.
    Arrow(Box<Kind>, Box<Kind>),
}

// ---------------------------------------------------------------------------
// Constraints (typeclasses / traits)
// ---------------------------------------------------------------------------

/// A typeclass constraint: `Ord a`, `Functor f`, `Serialize a`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Constraint {
    /// The typeclass / trait name.
    pub class: String,
    /// The type arguments the constraint applies to.
    pub args: Vec<Ty>,
}

// ---------------------------------------------------------------------------
// Effects
// ---------------------------------------------------------------------------

/// A set of effects on a function arrow. Empty means pure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectSet(pub Vec<Effect>);

impl EffectSet {
    pub fn pure() -> Self {
        Self(Vec::new())
    }

    pub fn is_pure(&self) -> bool {
        self.0.is_empty()
    }
}

/// An algebraic effect that a function may perform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effect {
    /// File system, network, system calls.
    IO,
    /// Requires an async runtime / is non-blocking.
    Async,
    /// Can fail with the given error type.
    Fail(Ty),
    /// Reads or modifies mutable state of the given type.
    State(Ty),
    /// Uses randomness (non-deterministic).
    Rand,
    /// Heap allocation (relevant for embedded / wasm).
    Alloc,
    /// Extensible: user-defined or language-specific effects.
    Named(String),
}

// ---------------------------------------------------------------------------
// Function signature (replaces the old FunctionSig)
// ---------------------------------------------------------------------------

/// A named function with a universal type and optional description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionSig {
    /// The function name.
    pub name: String,
    /// The full type (typically a `Forall` or `Fun`).
    pub ty: Ty,
    /// Natural-language description extracted by the LLM.
    pub description: String,
}

/// A module's exported signatures, extracted from a single file or module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleSignature {
    /// Path of the source file relative to the repo root.
    pub path: String,
    /// Exported function signatures.
    pub functions: Vec<FunctionSig>,
}

// ---------------------------------------------------------------------------
// Display — canonical text form
// ---------------------------------------------------------------------------

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Prim(p) => write!(f, "{p}"),
            Ty::Var(v) => write!(f, "{}", v.0),
            Ty::Named(n) => write!(f, "{n}"),

            Ty::Fun {
                param,
                effects,
                ret,
            } => {
                // Parenthesize function params that are themselves functions.
                match param.as_ref() {
                    Ty::Fun { .. } | Ty::Forall { .. } => write!(f, "({param})")?,
                    _ => write!(f, "{param}")?,
                }
                if effects.is_pure() {
                    write!(f, " -> ")?;
                } else {
                    write!(f, " ->{effects} ")?;
                }
                write!(f, "{ret}")
            }

            Ty::App { con, args } => {
                write!(f, "{con}")?;
                for arg in args {
                    match arg {
                        // Parenthesize complex args
                        Ty::App { args: inner, .. } if !inner.is_empty() => {
                            write!(f, " ({arg})")?;
                        }
                        Ty::Fun { .. } | Ty::Forall { .. } => write!(f, " ({arg})")?,
                        _ => write!(f, " {arg}")?,
                    }
                }
                Ok(())
            }

            Ty::Tuple(elems) => {
                write!(f, "(")?;
                for (i, elem) in elems.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{elem}")?;
                }
                write!(f, ")")
            }

            Ty::Record(fields) => {
                write!(f, "{{ ")?;
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: {}", field.name, field.ty)?;
                }
                write!(f, " }}")
            }

            Ty::Variant(cases) => {
                write!(f, "< ")?;
                for (i, case) in cases.iter().enumerate() {
                    if i > 0 {
                        write!(f, " | ")?;
                    }
                    write!(f, "{}: {}", case.name, case.ty)?;
                }
                write!(f, " >")
            }

            Ty::Forall {
                vars,
                constraints,
                body,
            } => {
                write!(f, "forall ")?;
                for (i, binding) in vars.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{binding}")?;
                }
                write!(f, ".")?;
                if !constraints.is_empty() {
                    write!(f, " ")?;
                    if constraints.len() == 1 {
                        write!(f, "{}", constraints[0])?;
                    } else {
                        write!(f, "(")?;
                        for (i, c) in constraints.iter().enumerate() {
                            if i > 0 {
                                write!(f, ", ")?;
                            }
                            write!(f, "{c}")?;
                        }
                        write!(f, ")")?;
                    }
                    write!(f, " => ")?;
                } else {
                    write!(f, " ")?;
                }
                write!(f, "{body}")
            }
        }
    }
}

impl fmt::Display for Prim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Prim::Int => write!(f, "Int"),
            Prim::Float => write!(f, "Float"),
            Prim::String => write!(f, "String"),
            Prim::Bool => write!(f, "Bool"),
            Prim::Bytes => write!(f, "Bytes"),
            Prim::Unit => write!(f, "Unit"),
            Prim::Never => write!(f, "Never"),
        }
    }
}

impl fmt::Display for TyVarBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            Kind::Type => write!(f, "{}", self.var.0),
            kind => write!(f, "({} : {kind})", self.var.0),
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Kind::Type => write!(f, "Type"),
            Kind::Arrow(from, to) => {
                match from.as_ref() {
                    Kind::Arrow(..) => write!(f, "({from})")?,
                    _ => write!(f, "{from}")?,
                }
                write!(f, " -> {to}")
            }
        }
    }
}

impl fmt::Display for Constraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.class)?;
        for arg in &self.args {
            match arg {
                Ty::App { args: inner, .. } if !inner.is_empty() => {
                    write!(f, " ({arg})")?;
                }
                Ty::Fun { .. } | Ty::Forall { .. } => write!(f, " ({arg})")?,
                _ => write!(f, " {arg}")?,
            }
        }
        Ok(())
    }
}

impl fmt::Display for EffectSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{")?;
        for (i, eff) in self.0.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{eff}")?;
        }
        write!(f, "}}")
    }
}

impl fmt::Display for Effect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Effect::IO => write!(f, "IO"),
            Effect::Async => write!(f, "Async"),
            Effect::Rand => write!(f, "Rand"),
            Effect::Alloc => write!(f, "Alloc"),
            Effect::Fail(ty) => {
                write!(f, "Fail ")?;
                match ty {
                    Ty::App { args, .. } if !args.is_empty() => write!(f, "({ty})"),
                    Ty::Fun { .. } | Ty::Forall { .. } => write!(f, "({ty})"),
                    _ => write!(f, "{ty}"),
                }
            }
            Effect::State(ty) => {
                write!(f, "State ")?;
                match ty {
                    Ty::App { args, .. } if !args.is_empty() => write!(f, "({ty})"),
                    Ty::Fun { .. } | Ty::Forall { .. } => write!(f, "({ty})"),
                    _ => write!(f, "{ty}"),
                }
            }
            Effect::Named(name) => write!(f, "{name}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Convenience constructors
// ---------------------------------------------------------------------------

impl Ty {
    /// `a -> b` (pure function).
    pub fn fun(param: Ty, ret: Ty) -> Self {
        Ty::Fun {
            param: Box::new(param),
            effects: EffectSet::pure(),
            ret: Box::new(ret),
        }
    }

    /// `a ->{effects} b` (effectful function).
    pub fn effectful(param: Ty, effects: Vec<Effect>, ret: Ty) -> Self {
        Ty::Fun {
            param: Box::new(param),
            effects: EffectSet(effects),
            ret: Box::new(ret),
        }
    }

    /// `Con a b c` (type constructor application).
    pub fn app(con: impl Into<String>, args: Vec<Ty>) -> Self {
        Ty::App {
            con: Box::new(Ty::Named(con.into())),
            args,
        }
    }

    /// Shorthand for `Ty::Var`.
    pub fn var(name: impl Into<String>) -> Self {
        Ty::Var(TyVar(name.into()))
    }

    /// Shorthand for `Ty::Named`.
    pub fn named(name: impl Into<String>) -> Self {
        Ty::Named(name.into())
    }

    /// Rename all bound type variables to canonical names (`t0`, `t1`, …)
    /// so that alpha-equivalent types produce identical text.
    ///
    /// ```
    /// use agentcoderepo_types::parse::parse_ty;
    ///
    /// let a = parse_ty("forall a. a -> a").unwrap().alpha_normalize();
    /// let b = parse_ty("forall b. b -> b").unwrap().alpha_normalize();
    /// assert_eq!(a, b);
    /// assert_eq!(a.to_string(), "forall t0. t0 -> t0");
    /// ```
    pub fn alpha_normalize(&self) -> Ty {
        self.normalize_vars(&mut 0, &HashMap::new())
    }

    fn normalize_vars(&self, counter: &mut usize, env: &HashMap<String, String>) -> Ty {
        match self {
            Ty::Var(TyVar(name)) => {
                if let Some(canonical) = env.get(name) {
                    Ty::Var(TyVar(canonical.clone()))
                } else {
                    self.clone()
                }
            }

            Ty::Forall {
                vars,
                constraints,
                body,
            } => {
                let mut env = env.clone();
                let new_vars = vars
                    .iter()
                    .map(|b| {
                        let canonical = format!("t{counter}");
                        *counter += 1;
                        env.insert(b.var.0.clone(), canonical.clone());
                        TyVarBinding {
                            var: TyVar(canonical),
                            kind: b.kind.clone(),
                        }
                    })
                    .collect();
                let new_constraints = constraints
                    .iter()
                    .map(|c| Constraint {
                        class: c.class.clone(),
                        args: c.args.iter().map(|a| a.normalize_vars(counter, &env)).collect(),
                    })
                    .collect();
                Ty::Forall {
                    vars: new_vars,
                    constraints: new_constraints,
                    body: Box::new(body.normalize_vars(counter, &env)),
                }
            }

            Ty::Fun {
                param,
                effects,
                ret,
            } => Ty::Fun {
                param: Box::new(param.normalize_vars(counter, env)),
                effects: EffectSet(
                    effects
                        .0
                        .iter()
                        .map(|e| e.normalize_vars(counter, env))
                        .collect(),
                ),
                ret: Box::new(ret.normalize_vars(counter, env)),
            },

            Ty::App { con, args } => Ty::App {
                con: Box::new(con.normalize_vars(counter, env)),
                args: args.iter().map(|a| a.normalize_vars(counter, env)).collect(),
            },

            Ty::Record(fields) => Ty::Record(
                fields
                    .iter()
                    .map(|f| Field {
                        name: f.name.clone(),
                        ty: f.ty.normalize_vars(counter, env),
                    })
                    .collect(),
            ),

            Ty::Variant(cases) => Ty::Variant(
                cases
                    .iter()
                    .map(|f| Field {
                        name: f.name.clone(),
                        ty: f.ty.normalize_vars(counter, env),
                    })
                    .collect(),
            ),

            Ty::Tuple(elems) => {
                Ty::Tuple(elems.iter().map(|e| e.normalize_vars(counter, env)).collect())
            }

            Ty::Prim(_) | Ty::Named(_) => self.clone(),
        }
    }
}

impl Effect {
    fn normalize_vars(&self, counter: &mut usize, env: &HashMap<String, String>) -> Effect {
        match self {
            Effect::Fail(ty) => Effect::Fail(ty.normalize_vars(counter, env)),
            Effect::State(ty) => Effect::State(ty.normalize_vars(counter, env)),
            other => other.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_pure_sort() {
        // forall a. Ord a => List a -> List a
        let ty = Ty::Forall {
            vars: vec![TyVarBinding::simple("a")],
            constraints: vec![Constraint {
                class: "Ord".into(),
                args: vec![Ty::var("a")],
            }],
            body: Box::new(Ty::fun(
                Ty::app("List", vec![Ty::var("a")]),
                Ty::app("List", vec![Ty::var("a")]),
            )),
        };
        assert_eq!(ty.to_string(), "forall a. Ord a => List a -> List a");
    }

    #[test]
    fn display_effectful_http_get() {
        // Text ->{IO, Fail HttpError} Response
        let ty = Ty::effectful(
            Ty::Prim(Prim::String),
            vec![Effect::IO, Effect::Fail(Ty::named("HttpError"))],
            Ty::named("Response"),
        );
        assert_eq!(
            ty.to_string(),
            "String ->{IO, Fail HttpError} Response"
        );
    }

    #[test]
    fn display_higher_kinded_functor_map() {
        // forall (f : Type -> Type) a b. Functor f => (a -> b) -> f a -> f b
        let ty = Ty::Forall {
            vars: vec![
                TyVarBinding::higher("f", Kind::Arrow(Box::new(Kind::Type), Box::new(Kind::Type))),
                TyVarBinding::simple("a"),
                TyVarBinding::simple("b"),
            ],
            constraints: vec![Constraint {
                class: "Functor".into(),
                args: vec![Ty::var("f")],
            }],
            body: Box::new(Ty::fun(
                Ty::fun(Ty::var("a"), Ty::var("b")),
                Ty::fun(
                    Ty::App {
                        con: Box::new(Ty::var("f")),
                        args: vec![Ty::var("a")],
                    },
                    Ty::App {
                        con: Box::new(Ty::var("f")),
                        args: vec![Ty::var("b")],
                    },
                ),
            )),
        };
        assert_eq!(
            ty.to_string(),
            "forall (f : Type -> Type) a b. Functor f => (a -> b) -> f a -> f b"
        );
    }

    #[test]
    fn display_record() {
        let ty = Ty::Record(vec![
            Field {
                name: "name".into(),
                ty: Ty::Prim(Prim::String),
            },
            Field {
                name: "age".into(),
                ty: Ty::Prim(Prim::Int),
            },
        ]);
        assert_eq!(ty.to_string(), "{ name: String, age: Int }");
    }

    #[test]
    fn display_variant() {
        let ty = Ty::Variant(vec![
            Field {
                name: "Ok".into(),
                ty: Ty::var("a"),
            },
            Field {
                name: "Err".into(),
                ty: Ty::var("e"),
            },
        ]);
        assert_eq!(ty.to_string(), "< Ok: a | Err: e >");
    }

    #[test]
    fn display_tuple() {
        let ty = Ty::Tuple(vec![Ty::Prim(Prim::Int), Ty::Prim(Prim::String), Ty::Prim(Prim::Bool)]);
        assert_eq!(ty.to_string(), "(Int, String, Bool)");
    }

    #[test]
    fn display_nested_app_parenthesized() {
        // Map String (List Int)
        let ty = Ty::app("Map", vec![Ty::Prim(Prim::String), Ty::app("List", vec![Ty::Prim(Prim::Int)])]);
        assert_eq!(ty.to_string(), "Map String (List Int)");
    }

    #[test]
    fn display_multiple_constraints() {
        // forall a. (Ord a, Show a) => a -> String
        let ty = Ty::Forall {
            vars: vec![TyVarBinding::simple("a")],
            constraints: vec![
                Constraint {
                    class: "Ord".into(),
                    args: vec![Ty::var("a")],
                },
                Constraint {
                    class: "Show".into(),
                    args: vec![Ty::var("a")],
                },
            ],
            body: Box::new(Ty::fun(Ty::var("a"), Ty::Prim(Prim::String))),
        };
        assert_eq!(
            ty.to_string(),
            "forall a. (Ord a, Show a) => a -> String"
        );
    }

    #[test]
    fn display_kind_arrow() {
        let k = Kind::Arrow(
            Box::new(Kind::Type),
            Box::new(Kind::Arrow(Box::new(Kind::Type), Box::new(Kind::Type))),
        );
        assert_eq!(k.to_string(), "Type -> Type -> Type");
    }

    #[test]
    fn pure_effect_set() {
        let eff = EffectSet::pure();
        assert!(eff.is_pure());
        assert_eq!(eff.to_string(), "{}");
    }

    // --- Alpha normalization tests ---

    #[test]
    fn alpha_equiv_identity() {
        let a = crate::parse::parse_ty("forall a. a -> a").unwrap();
        let b = crate::parse::parse_ty("forall b. b -> b").unwrap();
        assert_ne!(a, b); // structurally different
        assert_eq!(a.alpha_normalize(), b.alpha_normalize());
        assert_eq!(a.alpha_normalize().to_string(), "forall t0. t0 -> t0");
    }

    #[test]
    fn alpha_equiv_sort() {
        let a = crate::parse::parse_ty("forall a. Ord a => List a -> List a").unwrap();
        let b = crate::parse::parse_ty("forall x. Ord x => List x -> List x").unwrap();
        assert_eq!(a.alpha_normalize(), b.alpha_normalize());
    }

    #[test]
    fn alpha_equiv_multi_var() {
        let a = crate::parse::parse_ty("forall a b. (a -> b) -> List a -> List b").unwrap();
        let b = crate::parse::parse_ty("forall x y. (x -> y) -> List x -> List y").unwrap();
        assert_eq!(a.alpha_normalize(), b.alpha_normalize());
        assert_eq!(
            a.alpha_normalize().to_string(),
            "forall t0 t1. (t0 -> t1) -> List t0 -> List t1"
        );
    }

    #[test]
    fn alpha_equiv_higher_kinded() {
        let a = crate::parse::parse_ty(
            "forall (f : Type -> Type) a b. Functor f => (a -> b) -> f a -> f b",
        )
        .unwrap();
        let b = crate::parse::parse_ty(
            "forall (g : Type -> Type) x y. Functor g => (x -> y) -> g x -> g y",
        )
        .unwrap();
        assert_eq!(a.alpha_normalize(), b.alpha_normalize());
    }

    #[test]
    fn alpha_equiv_nested_forall() {
        let a = crate::parse::parse_ty("forall a. a -> (forall b. b -> a)").unwrap();
        let b = crate::parse::parse_ty("forall x. x -> (forall y. y -> x)").unwrap();
        assert_eq!(a.alpha_normalize(), b.alpha_normalize());
        assert_eq!(
            a.alpha_normalize().to_string(),
            "forall t0. t0 -> forall t1. t1 -> t0"
        );
    }

    #[test]
    fn alpha_equiv_effectful() {
        let a = crate::parse::parse_ty("forall e. String ->{Fail e} Int").unwrap();
        let b = crate::parse::parse_ty("forall x. String ->{Fail x} Int").unwrap();
        assert_eq!(a.alpha_normalize(), b.alpha_normalize());
    }

    #[test]
    fn alpha_no_forall_unchanged() {
        let ty = crate::parse::parse_ty("Int -> String").unwrap();
        assert_eq!(ty.alpha_normalize(), ty);
    }

    #[test]
    fn alpha_different_structure_not_equal() {
        let a = crate::parse::parse_ty("forall a. a -> a").unwrap();
        let b = crate::parse::parse_ty("forall a b. a -> b").unwrap();
        assert_ne!(a.alpha_normalize(), b.alpha_normalize());
    }

    #[test]
    fn serde_roundtrip() {
        let ty = Ty::Forall {
            vars: vec![TyVarBinding::simple("a")],
            constraints: vec![Constraint {
                class: "Ord".into(),
                args: vec![Ty::var("a")],
            }],
            body: Box::new(Ty::fun(
                Ty::app("List", vec![Ty::var("a")]),
                Ty::app("List", vec![Ty::var("a")]),
            )),
        };
        let json = serde_json::to_string(&ty).unwrap();
        let roundtripped: Ty = serde_json::from_str(&json).unwrap();
        assert_eq!(ty, roundtripped);
    }
}

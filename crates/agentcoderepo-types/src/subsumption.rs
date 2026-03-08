//! Type subsumption (backward compatibility) checking.
//!
//! Determines whether a new type can safely replace an old type without
//! breaking existing callers. Based on System F subsumption rules:
//!
//! - **Generalization is safe**: `forall a. a -> a` can replace `Int -> Int`
//!   (the new type instantiates to the old one).
//! - **Specialization is breaking**: `Int -> Int` cannot replace `forall a. a -> a`
//!   (callers passing `String` would break).
//! - **Removing constraints is safe**: `forall a. List a -> List a` can replace
//!   `forall a. Ord a => List a -> List a` (less restrictive).
//! - **Adding constraints is breaking**.
//! - **Removing effects is safe**: a pure function can replace an effectful one.
//! - **Adding effects is breaking**.

use std::collections::HashMap;

use crate::ty::{Constraint, Effect, EffectSet, Field, Kind, Ty, TyVar, TyVarBinding};

/// Result of a compatibility check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compat {
    /// The new type can be used everywhere the old type was used.
    Compatible,
    /// The new type cannot safely replace the old type.
    Breaking,
}

/// Check whether `new_ty` is a backward-compatible replacement for `old_ty`.
///
/// Returns `Compatible` if every use site of the old type would still work
/// with the new type (i.e., `new_ty` subsumes `old_ty`).
pub fn check_compat(old_ty: &Ty, new_ty: &Ty) -> Compat {
    // Alpha-normalize both so variable names don't interfere
    let old = old_ty.alpha_normalize();
    let new = new_ty.alpha_normalize();

    if old == new {
        return Compat::Compatible;
    }

    // Try to show that new subsumes old:
    // Strip foralls from new, collecting variables we can instantiate,
    // then try to match the body against old.
    let (new_vars, new_constraints, new_body) = peel_forall(&new);
    let (old_vars, old_constraints, old_body) = peel_forall(&old);

    // Build the set of variables from `new` that are free to instantiate.
    // Variables from `old` are rigid (must match exactly, modulo renaming).
    let mut subst: HashMap<String, Ty> = HashMap::new();

    // New's universally quantified vars can be instantiated
    let instantiable: std::collections::HashSet<String> =
        new_vars.iter().map(|b| b.var.0.clone()).collect();
    let rigid: std::collections::HashSet<String> =
        old_vars.iter().map(|b| b.var.0.clone()).collect();

    // Try matching new_body against old_body
    if !match_ty(&new_body, &old_body, &instantiable, &rigid, &mut subst) {
        return Compat::Breaking;
    }

    // Check kind compatibility for matched variables
    for new_bind in &new_vars {
        if let Some(matched_ty) = subst.get(&new_bind.var.0) {
            // If new var has higher kind, the matched type must be compatible
            // For v1: just check that if the var was matched, kinds are consistent
            if new_bind.kind != Kind::Type {
                // Higher-kinded variable was instantiated — check it was matched
                // to something reasonable (a var or a named constructor)
                match matched_ty {
                    Ty::Var(_) | Ty::Named(_) | Ty::Prim(_) => {}
                    _ => return Compat::Breaking,
                }
            }
        }
    }

    // Check constraints: new's constraints (after substitution) must be
    // a subset of old's. If new requires MORE constraints, that's breaking.
    // If new requires FEWER, that's fine (less restrictive).
    let new_constraints_subst: Vec<Constraint> = new_constraints
        .iter()
        .map(|c| apply_constraint_subst(c, &subst))
        .collect();

    for nc in &new_constraints_subst {
        if !old_constraints.iter().any(|oc| constraints_match(nc, oc)) {
            // New requires a constraint that old didn't — breaking only if
            // the constraint involves rigid variables. If it involves only
            // instantiated variables, it's fine (the instantiation satisfies it).
            let involves_rigid = nc.args.iter().any(|a| mentions_any(a, &rigid));
            if involves_rigid {
                return Compat::Breaking;
            }
        }
    }

    Compat::Compatible
}

// ---------------------------------------------------------------------------
// Forall peeling
// ---------------------------------------------------------------------------

/// Strip outer `Forall` wrappers, collecting all bindings and constraints.
fn peel_forall(ty: &Ty) -> (Vec<TyVarBinding>, Vec<Constraint>, Ty) {
    match ty {
        Ty::Forall {
            vars,
            constraints,
            body,
        } => {
            let (inner_vars, inner_constraints, inner_body) = peel_forall(body);
            let mut all_vars = vars.clone();
            all_vars.extend(inner_vars);
            let mut all_constraints = constraints.clone();
            all_constraints.extend(inner_constraints);
            (all_vars, all_constraints, inner_body)
        }
        other => (Vec::new(), Vec::new(), other.clone()),
    }
}

// ---------------------------------------------------------------------------
// One-way matching (pattern matching, not full unification)
// ---------------------------------------------------------------------------

/// Try to match `pattern` against `target`, filling in `subst` for
/// instantiable variables. Rigid variables must match exactly.
///
/// Returns true if matching succeeds.
fn match_ty(
    pattern: &Ty,
    target: &Ty,
    instantiable: &std::collections::HashSet<String>,
    rigid: &std::collections::HashSet<String>,
    subst: &mut HashMap<String, Ty>,
) -> bool {
    match (pattern, target) {
        // Instantiable variable: can be matched to anything
        (Ty::Var(TyVar(name)), _) if instantiable.contains(name.as_str()) => {
            if let Some(existing) = subst.get(name) {
                // Already matched — must be consistent
                existing == target
            } else {
                subst.insert(name.clone(), target.clone());
                true
            }
        }

        // Rigid variable: must match the same rigid variable
        (Ty::Var(TyVar(a)), Ty::Var(TyVar(b))) if rigid.contains(a.as_str()) => a == b,

        // Same-shape matching
        (Ty::Prim(a), Ty::Prim(b)) => a == b,
        (Ty::Named(a), Ty::Named(b)) => a == b,
        (Ty::Var(TyVar(a)), Ty::Var(TyVar(b))) => a == b,

        (
            Ty::Fun {
                param: p1,
                effects: e1,
                ret: r1,
            },
            Ty::Fun {
                param: p2,
                effects: e2,
                ret: r2,
            },
        ) => {
            // Function types: covariant in return, contravariant in param.
            // For subsumption of the whole signature, we match structurally
            // (the forall peeling handles the variance).
            match_ty(p1, p2, instantiable, rigid, subst)
                && match_effects(e1, e2, instantiable, rigid, subst)
                && match_ty(r1, r2, instantiable, rigid, subst)
        }

        (
            Ty::App {
                con: c1,
                args: a1,
            },
            Ty::App {
                con: c2,
                args: a2,
            },
        ) => {
            if a1.len() != a2.len() {
                return false;
            }
            match_ty(c1, c2, instantiable, rigid, subst)
                && a1
                    .iter()
                    .zip(a2.iter())
                    .all(|(x, y)| match_ty(x, y, instantiable, rigid, subst))
        }

        (Ty::Tuple(a), Ty::Tuple(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|(x, y)| match_ty(x, y, instantiable, rigid, subst))
        }

        (Ty::Record(a), Ty::Record(b)) => match_fields(a, b, instantiable, rigid, subst),
        (Ty::Variant(a), Ty::Variant(b)) => match_fields(a, b, instantiable, rigid, subst),

        // Forall inside the body: match structurally
        // (nested foralls after peeling means this is a rank-2+ type)
        (
            Ty::Forall {
                vars: v1,
                constraints: c1,
                body: b1,
            },
            Ty::Forall {
                vars: v2,
                constraints: c2,
                body: b2,
            },
        ) => {
            // Same number of bindings, same kinds
            if v1.len() != v2.len() {
                return false;
            }
            for (a, b) in v1.iter().zip(v2.iter()) {
                if a.kind != b.kind {
                    return false;
                }
            }
            // Match constraints and body (treating inner vars as rigid)
            if c1.len() != c2.len() {
                return false;
            }
            for (a, b) in c1.iter().zip(c2.iter()) {
                if a.class != b.class || a.args.len() != b.args.len() {
                    return false;
                }
            }
            match_ty(b1, b2, instantiable, rigid, subst)
        }

        _ => false,
    }
}

/// Match effect sets. The new type's effects can be a subset of the old's
/// (removing effects is safe — function is "purer").
fn match_effects(
    new_effects: &EffectSet,
    old_effects: &EffectSet,
    instantiable: &std::collections::HashSet<String>,
    rigid: &std::collections::HashSet<String>,
    subst: &mut HashMap<String, Ty>,
) -> bool {
    // Every effect in the new set must appear in the old set (or match via substitution).
    // The old set can have extra effects (the old function was allowed to do more).
    // Wait — this is backwards for subsumption. If new replaces old:
    // - Old callers expect the function MAY have effects E_old
    // - If new has FEWER effects, that's fine (it does less)
    // - If new has MORE effects, callers aren't prepared for them — breaking
    //
    // So: new_effects ⊆ old_effects (after substitution)
    for new_eff in &new_effects.0 {
        let matched = old_effects.0.iter().any(|old_eff| {
            match_effect(new_eff, old_eff, instantiable, rigid, subst)
        });
        if !matched {
            return false;
        }
    }
    true
}

fn match_effect(
    new: &Effect,
    old: &Effect,
    instantiable: &std::collections::HashSet<String>,
    rigid: &std::collections::HashSet<String>,
    subst: &mut HashMap<String, Ty>,
) -> bool {
    match (new, old) {
        (Effect::IO, Effect::IO)
        | (Effect::Async, Effect::Async)
        | (Effect::Rand, Effect::Rand)
        | (Effect::Alloc, Effect::Alloc) => true,
        (Effect::Fail(a), Effect::Fail(b)) | (Effect::State(a), Effect::State(b)) => {
            match_ty(a, b, instantiable, rigid, subst)
        }
        (Effect::Named(a), Effect::Named(b)) => a == b,
        _ => false,
    }
}

fn match_fields(
    a: &[Field],
    b: &[Field],
    instantiable: &std::collections::HashSet<String>,
    rigid: &std::collections::HashSet<String>,
    subst: &mut HashMap<String, Ty>,
) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(fa, fb)| {
        fa.name == fb.name && match_ty(&fa.ty, &fb.ty, instantiable, rigid, subst)
    })
}

// ---------------------------------------------------------------------------
// Constraint helpers
// ---------------------------------------------------------------------------

fn apply_subst(ty: &Ty, subst: &HashMap<String, Ty>) -> Ty {
    match ty {
        Ty::Var(TyVar(name)) => {
            if let Some(replacement) = subst.get(name) {
                replacement.clone()
            } else {
                ty.clone()
            }
        }
        Ty::Fun {
            param,
            effects,
            ret,
        } => Ty::Fun {
            param: Box::new(apply_subst(param, subst)),
            effects: EffectSet(
                effects
                    .0
                    .iter()
                    .map(|e| apply_effect_subst(e, subst))
                    .collect(),
            ),
            ret: Box::new(apply_subst(ret, subst)),
        },
        Ty::App { con, args } => Ty::App {
            con: Box::new(apply_subst(con, subst)),
            args: args.iter().map(|a| apply_subst(a, subst)).collect(),
        },
        Ty::Tuple(elems) => Ty::Tuple(elems.iter().map(|e| apply_subst(e, subst)).collect()),
        Ty::Record(fields) => Ty::Record(
            fields
                .iter()
                .map(|f| Field {
                    name: f.name.clone(),
                    ty: apply_subst(&f.ty, subst),
                })
                .collect(),
        ),
        Ty::Variant(cases) => Ty::Variant(
            cases
                .iter()
                .map(|f| Field {
                    name: f.name.clone(),
                    ty: apply_subst(&f.ty, subst),
                })
                .collect(),
        ),
        Ty::Forall {
            vars,
            constraints,
            body,
        } => {
            // Don't substitute into bound variables
            let bound: std::collections::HashSet<&str> =
                vars.iter().map(|b| b.var.0.as_str()).collect();
            let filtered: HashMap<String, Ty> = subst
                .iter()
                .filter(|(k, _)| !bound.contains(k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            Ty::Forall {
                vars: vars.clone(),
                constraints: constraints
                    .iter()
                    .map(|c| apply_constraint_subst(c, &filtered))
                    .collect(),
                body: Box::new(apply_subst(body, &filtered)),
            }
        }
        Ty::Prim(_) | Ty::Named(_) => ty.clone(),
    }
}

fn apply_effect_subst(effect: &Effect, subst: &HashMap<String, Ty>) -> Effect {
    match effect {
        Effect::Fail(ty) => Effect::Fail(apply_subst(ty, subst)),
        Effect::State(ty) => Effect::State(apply_subst(ty, subst)),
        other => other.clone(),
    }
}

fn apply_constraint_subst(c: &Constraint, subst: &HashMap<String, Ty>) -> Constraint {
    Constraint {
        class: c.class.clone(),
        args: c.args.iter().map(|a| apply_subst(a, subst)).collect(),
    }
}

fn constraints_match(a: &Constraint, b: &Constraint) -> bool {
    a.class == b.class && a.args == b.args
}

/// Check if a type mentions any variable in the given set.
fn mentions_any(ty: &Ty, vars: &std::collections::HashSet<String>) -> bool {
    match ty {
        Ty::Var(TyVar(name)) => vars.contains(name),
        Ty::Fun {
            param,
            effects,
            ret,
        } => {
            mentions_any(param, vars)
                || effects
                    .0
                    .iter()
                    .any(|e| match e {
                        Effect::Fail(t) | Effect::State(t) => mentions_any(t, vars),
                        _ => false,
                    })
                || mentions_any(ret, vars)
        }
        Ty::App { con, args } => {
            mentions_any(con, vars) || args.iter().any(|a| mentions_any(a, vars))
        }
        Ty::Tuple(elems) => elems.iter().any(|e| mentions_any(e, vars)),
        Ty::Record(fields) | Ty::Variant(fields) => {
            fields.iter().any(|f| mentions_any(&f.ty, vars))
        }
        Ty::Forall { body, .. } => mentions_any(body, vars),
        Ty::Prim(_) | Ty::Named(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_ty;

    fn compat(old: &str, new: &str) -> Compat {
        let old_ty = parse_ty(old).unwrap();
        let new_ty = parse_ty(new).unwrap();
        check_compat(&old_ty, &new_ty)
    }

    // --- Identical types ---

    #[test]
    fn identical_concrete() {
        assert_eq!(compat("Int -> Int", "Int -> Int"), Compat::Compatible);
    }

    #[test]
    fn identical_polymorphic() {
        assert_eq!(
            compat("forall a. a -> a", "forall a. a -> a"),
            Compat::Compatible
        );
    }

    #[test]
    fn alpha_equivalent() {
        assert_eq!(
            compat("forall a. a -> a", "forall b. b -> b"),
            Compat::Compatible
        );
    }

    // --- Generalization (non-breaking) ---

    #[test]
    fn generalize_concrete_to_polymorphic() {
        // forall a. a -> a can replace Int -> Int
        assert_eq!(
            compat("Int -> Int", "forall a. a -> a"),
            Compat::Compatible
        );
    }

    #[test]
    fn generalize_specific_list_to_polymorphic() {
        assert_eq!(
            compat("List Int -> List Int", "forall a. List a -> List a"),
            Compat::Compatible
        );
    }

    #[test]
    fn remove_constraint() {
        // Removing Ord constraint — less restrictive, safe
        assert_eq!(
            compat(
                "forall a. Ord a => List a -> List a",
                "forall a. List a -> List a"
            ),
            Compat::Compatible
        );
    }

    #[test]
    fn remove_effect() {
        // Removing IO effect — function became pure, safe
        assert_eq!(
            compat("String ->{IO} Int", "String -> Int"),
            Compat::Compatible
        );
    }

    // --- Specialization (breaking) ---

    #[test]
    fn specialize_polymorphic_to_concrete() {
        // Int -> Int cannot replace forall a. a -> a
        assert_eq!(
            compat("forall a. a -> a", "Int -> Int"),
            Compat::Breaking
        );
    }

    #[test]
    fn change_concrete_type() {
        assert_eq!(
            compat("Int -> Int", "String -> String"),
            Compat::Breaking
        );
    }

    #[test]
    fn add_constraint() {
        // Adding a constraint — more restrictive, breaking
        assert_eq!(
            compat(
                "forall a. List a -> List a",
                "forall a. Ord a => List a -> List a"
            ),
            Compat::Breaking
        );
    }

    #[test]
    fn add_effect() {
        // Adding IO effect — function now does I/O, breaking
        assert_eq!(
            compat("String -> Int", "String ->{IO} Int"),
            Compat::Breaking
        );
    }

    #[test]
    fn change_return_type() {
        assert_eq!(
            compat("Int -> Int", "Int -> String"),
            Compat::Breaking
        );
    }

    #[test]
    fn change_param_type() {
        assert_eq!(
            compat("Int -> String", "String -> String"),
            Compat::Breaking
        );
    }

    // --- Effect subsumption ---

    #[test]
    fn remove_one_of_two_effects() {
        // Removing one effect while keeping another
        assert_eq!(
            compat(
                "String ->{IO, Fail Error} Int",
                "String ->{Fail Error} Int"
            ),
            Compat::Compatible
        );
    }

    #[test]
    fn keep_effects_unchanged() {
        assert_eq!(
            compat(
                "String ->{IO, Fail Error} Int",
                "String ->{IO, Fail Error} Int"
            ),
            Compat::Compatible
        );
    }

    // --- Complex cases ---

    #[test]
    fn generalize_multi_arg() {
        assert_eq!(
            compat(
                "Int -> String -> Bool",
                "forall a b c. a -> b -> c"
            ),
            Compat::Compatible
        );
    }

    #[test]
    fn generalize_with_type_constructor() {
        assert_eq!(
            compat(
                "Map String Int -> List Int",
                "forall k v. Map k v -> List v"
            ),
            Compat::Compatible
        );
    }

    #[test]
    fn incompatible_type_constructor_args() {
        // Map String Int -> List Int vs forall k v. Map k v -> List k
        // Can't unify: v=Int from Map, but List k requires k=Int,
        // while Map k v requires k=String. k can't be both.
        assert_eq!(
            compat(
                "Map String Int -> List Int",
                "forall k v. Map k v -> List k"
            ),
            Compat::Breaking
        );
    }
}

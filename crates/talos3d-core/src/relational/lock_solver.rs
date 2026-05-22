//! Bounded local lock solver (PP-RPS-6).
//!
//! Per `RELATIONAL_PARAMETRIC_SUBSTRATE_AGREEMENT.md` ("Bounded Lock Solver"):
//! when the user **locks** one scalar derived observable and frees **one**
//! driver, invert the composed relation over the component-local acyclic scalar
//! graph. Use symbolic inversion when the relation is affine in the free driver;
//! otherwise deterministic numeric root-finding, but **only** when the relation
//! declares monotonicity, bounds, units, and tolerance.
//!
//! Anything outside that envelope — multiple locks, multiple free drivers,
//! inequalities/contact, cycles, discontinuities, or missing bounds — stops with
//! an explainable finding and an `AssumptionLog` note marked "needs general
//! solver". This is deliberately **not** a general simultaneous solver.

use super::param_expr::{Env, EvalError, Quantity, ScalarExpr, Unit};

/// Declared monotonicity of the locked observable w.r.t. the free driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Monotonicity {
    Increasing,
    Decreasing,
}

/// What the caller declares about the free driver, enabling numeric solving.
#[derive(Debug, Clone)]
pub struct SolveSpec {
    /// Unit of the free driver value.
    pub free_unit: Unit,
    /// Search bounds for the free driver (required for numeric root-finding).
    pub bounds: Option<(f64, f64)>,
    /// Declared monotonicity (required for numeric root-finding).
    pub monotonic: Option<Monotonicity>,
    /// Absolute tolerance on the locked observable.
    pub tolerance: f64,
}

/// A single lock request: hold `expr` (a derived value) at `locked_value` by
/// freeing the single driver `free_driver`.
#[derive(Debug, Clone)]
pub struct LockRequest<'a> {
    pub expr: &'a ScalarExpr,
    pub locked_value: Quantity,
    pub free_driver: &'a str,
    /// Current values of all other drivers (the free driver's entry, if any,
    /// is overwritten during solving).
    pub env: Env,
    pub spec: SolveSpec,
    /// Count of additional locks beyond this one (must be 0 for v1).
    pub extra_locks: usize,
    /// Count of additional free drivers beyond this one (must be 0 for v1).
    pub extra_free: usize,
}

/// An `AssumptionLog`-shaped note for a deferral (ADR-042 §11/§13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssumptionNote {
    pub summary: String,
    pub needs_general_solver: bool,
}

/// Outcome of a lock solve.
#[derive(Debug, Clone, PartialEq)]
pub enum LockOutcome {
    /// Solved: the free driver value (in `spec.free_unit`) plus how it was found.
    Solved {
        free_value: Quantity,
        method: SolveMethod,
        iterations: u32,
    },
    /// Refused with an explainable finding and an assumption-log note.
    Refused {
        finding: String,
        note: AssumptionNote,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolveMethod {
    SymbolicAffine,
    NumericBisection,
}

fn refuse(reason: impl Into<String>) -> LockOutcome {
    let reason = reason.into();
    LockOutcome::Refused {
        finding: reason.clone(),
        note: AssumptionNote {
            summary: format!("lock deferred: {reason}"),
            needs_general_solver: true,
        },
    }
}

/// Evaluate the locked expression with the free driver set to `x`.
fn eval_at(req: &LockRequest<'_>, x: f64) -> Result<f64, EvalError> {
    let mut env = req.env.clone();
    env.insert(
        req.free_driver.to_string(),
        Quantity {
            value: x,
            unit: req.spec.free_unit,
        },
    );
    let q = req.expr.eval(&env)?;
    if q.unit != req.locked_value.unit {
        return Err(EvalError::UnitMismatch {
            op: "lock",
            lhs: q.unit,
            rhs: req.locked_value.unit,
        });
    }
    Ok(q.value)
}

/// Solve a single lock by inverting the composed relation.
pub fn solve_lock(req: &LockRequest<'_>) -> LockOutcome {
    // V1 envelope: exactly one lock, exactly one free driver.
    if req.extra_locks > 0 || req.extra_free > 0 {
        return refuse(format!(
            "multiple locks ({}) or free drivers ({}) — single-lock/single-free only",
            req.extra_locks + 1,
            req.extra_free + 1
        ));
    }
    // The free driver must actually influence the expression.
    if !req.expr.dependencies().contains(req.free_driver) {
        return refuse(format!(
            "locked value does not depend on free driver '{}'",
            req.free_driver
        ));
    }
    let target = req.locked_value.value;

    // --- 1. Symbolic affine fast path ------------------------------------
    // Only when the relation is *structurally* affine in the free driver
    // (the free driver never appears inside trig/min/max/clamp/if, in a
    // denominator, or multiplied by another free-bearing term). Sampling-based
    // "looks linear" detection is unsafe (tan looks linear over a tiny range),
    // so we decide by structure, then invert exactly from two points.
    if is_affine_in(req.expr, req.free_driver) {
        let (s0, s1) = req.spec.bounds.unwrap_or((0.0, 1.0));
        let (s0, s1) = if (s1 - s0).abs() < 1e-12 {
            (0.0, 1.0)
        } else {
            (s0, s1)
        };
        let (g0, g1) = match (eval_at(req, s0), eval_at(req, s1)) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(e), _) | (_, Err(e)) => {
                return refuse(format!("evaluation error during solve: {e}"))
            }
        };
        let slope = (g1 - g0) / (s1 - s0);
        if slope.abs() < 1e-12 {
            return refuse("locked value is independent of (or flat in) the free driver");
        }
        let x = s0 + (target - g0) / slope;
        return LockOutcome::Solved {
            free_value: Quantity {
                value: x,
                unit: req.spec.free_unit,
            },
            method: SolveMethod::SymbolicAffine,
            iterations: 0,
        };
    }

    // --- 2. Numeric bisection (requires bounds + monotonicity + tolerance) -
    let Some((lo, hi)) = req.spec.bounds else {
        return refuse("relation is non-affine and no bounds were declared (numeric solve needs bounds + monotonicity + tolerance)");
    };
    if req.spec.monotonic.is_none() {
        return refuse("relation is non-affine and monotonicity was not declared");
    }
    let (mut lo, mut hi) = (lo, hi);
    let flo = match eval_at(req, lo) {
        Ok(v) => v - target,
        Err(e) => return refuse(format!("evaluation error: {e}")),
    };
    let fhi = match eval_at(req, hi) {
        Ok(v) => v - target,
        Err(e) => return refuse(format!("evaluation error: {e}")),
    };
    if flo == 0.0 {
        return solved_numeric(req, lo, 0);
    }
    if fhi == 0.0 {
        return solved_numeric(req, hi, 0);
    }
    if flo.signum() == fhi.signum() {
        return refuse("target is outside the declared bounds (no sign change)");
    }
    // Deterministic bisection to tolerance on the observable.
    let tol = req.spec.tolerance.max(1e-9);
    let mut iters = 0u32;
    while iters < 200 {
        iters += 1;
        let mid = 0.5 * (lo + hi);
        let fmid = match eval_at(req, mid) {
            Ok(v) => v - target,
            Err(e) => return refuse(format!("evaluation error: {e}")),
        };
        if fmid.abs() <= tol || (hi - lo).abs() <= tol.max(1e-12) {
            return solved_numeric(req, mid, iters);
        }
        if fmid.signum() == flo.signum() {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    refuse("numeric solve did not converge within iteration budget")
}

/// Does `expr` mention `free` anywhere?
fn contains(expr: &ScalarExpr, free: &str) -> bool {
    expr.dependencies().contains(free)
}

/// Is `expr` structurally affine (degree ≤ 1) in `free`, with `free` never
/// inside a nonlinear/discontinuous op? Conservative: returns false whenever it
/// cannot prove affineness.
fn is_affine_in(expr: &ScalarExpr, free: &str) -> bool {
    match expr {
        ScalarExpr::Lit { .. } => true,
        ScalarExpr::Param { .. } => true, // bare param: degree 0 or 1, both affine
        ScalarExpr::Add { lhs, rhs } | ScalarExpr::Sub { lhs, rhs } => {
            is_affine_in(lhs, free) && is_affine_in(rhs, free)
        }
        ScalarExpr::Neg { expr } => is_affine_in(expr, free),
        ScalarExpr::Mul { lhs, rhs } => {
            // affine only if the free-bearing side is affine and the other side
            // is constant in free (no free*free, which would be quadratic).
            (!contains(lhs, free) && is_affine_in(rhs, free))
                || (!contains(rhs, free) && is_affine_in(lhs, free))
        }
        ScalarExpr::Div { lhs, rhs } => {
            // free may not appear in the denominator (would be nonlinear).
            !contains(rhs, free) && is_affine_in(lhs, free)
        }
        // Nonlinear / discontinuous ops: affine only if free is absent.
        ScalarExpr::Sin { expr } | ScalarExpr::Cos { expr } | ScalarExpr::Tan { expr } => {
            !contains(expr, free)
        }
        ScalarExpr::Atan2 { y, x } => !contains(y, free) && !contains(x, free),
        ScalarExpr::Min { lhs, rhs } | ScalarExpr::Max { lhs, rhs } => {
            !contains(lhs, free) && !contains(rhs, free)
        }
        ScalarExpr::Clamp { expr, lo, hi } => {
            !contains(expr, free) && !contains(lo, free) && !contains(hi, free)
        }
        ScalarExpr::If { cond, then, els } => {
            !cond.dependencies().contains(free) && !contains(then, free) && !contains(els, free)
        }
    }
}

fn solved_numeric(req: &LockRequest<'_>, x: f64, iterations: u32) -> LockOutcome {
    LockOutcome::Solved {
        free_value: Quantity {
            value: x,
            unit: req.spec.free_unit,
        },
        method: SolveMethod::NumericBisection,
        iterations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relational::param_expr::ScalarExpr as E;

    fn b(e: E) -> Box<E> {
        Box::new(e)
    }

    fn env(pairs: &[(&str, Quantity)]) -> Env {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    // apex = heel + (span/2) * tan(pitch)
    fn apex_expr() -> E {
        E::Add {
            lhs: b(E::Param {
                name: "heel".into(),
            }),
            rhs: b(E::Mul {
                lhs: b(E::Div {
                    lhs: b(E::Param {
                        name: "span".into(),
                    }),
                    rhs: b(E::Lit {
                        q: Quantity::num(2.0),
                    }),
                }),
                rhs: b(E::Tan {
                    expr: b(E::Param {
                        name: "pitch".into(),
                    }),
                }),
            }),
        }
    }

    #[test]
    fn symbolic_affine_inversion_for_span() {
        // apex is affine in span (slope = tan(pitch)/2). Lock apex, free span.
        let expr = apex_expr();
        let req = LockRequest {
            expr: &expr,
            locked_value: Quantity::mm(2255.0),
            free_driver: "span",
            env: env(&[("heel", Quantity::mm(90.0)), ("pitch", Quantity::deg(30.0))]),
            spec: SolveSpec {
                free_unit: Unit::Mm,
                bounds: None,
                monotonic: None,
                tolerance: 0.5,
            },
            extra_locks: 0,
            extra_free: 0,
        };
        match solve_lock(&req) {
            LockOutcome::Solved {
                free_value, method, ..
            } => {
                assert_eq!(method, SolveMethod::SymbolicAffine);
                // heel 90 + span/2 * tan30 = 2255 -> span ~ 7500
                assert!(
                    (free_value.value - 7500.0).abs() < 1.0,
                    "span ~7500, got {}",
                    free_value.value
                );
                assert_eq!(free_value.unit, Unit::Mm);
            }
            other => panic!("expected solved, got {other:?}"),
        }
    }

    #[test]
    fn numeric_inversion_for_pitch_with_bounds() {
        // apex is NON-affine in pitch (tan). With bounds+monotonicity, numeric.
        let expr = apex_expr();
        let req = LockRequest {
            expr: &expr,
            locked_value: Quantity::mm(2255.0),
            free_driver: "pitch",
            env: env(&[("heel", Quantity::mm(90.0)), ("span", Quantity::mm(7500.0))]),
            spec: SolveSpec {
                free_unit: Unit::Deg,
                bounds: Some((1.0, 80.0)),
                monotonic: Some(Monotonicity::Increasing),
                tolerance: 0.1,
            },
            extra_locks: 0,
            extra_free: 0,
        };
        match solve_lock(&req) {
            LockOutcome::Solved {
                free_value,
                method,
                iterations,
            } => {
                assert_eq!(method, SolveMethod::NumericBisection);
                assert!(iterations > 0);
                // pitch that gives apex 2255 with span 7500, heel 90 is ~30deg
                assert!(
                    (free_value.value - 30.0).abs() < 0.2,
                    "pitch ~30, got {}",
                    free_value.value
                );
            }
            other => panic!("expected solved, got {other:?}"),
        }
    }

    #[test]
    fn missing_bounds_refuses_for_nonaffine() {
        let expr = apex_expr();
        let req = LockRequest {
            expr: &expr,
            locked_value: Quantity::mm(2255.0),
            free_driver: "pitch",
            env: env(&[("heel", Quantity::mm(90.0)), ("span", Quantity::mm(7500.0))]),
            spec: SolveSpec {
                free_unit: Unit::Deg,
                bounds: None, // <- no bounds
                monotonic: None,
                tolerance: 0.1,
            },
            extra_locks: 0,
            extra_free: 0,
        };
        match solve_lock(&req) {
            LockOutcome::Refused { note, .. } => assert!(note.needs_general_solver),
            other => panic!("expected refusal, got {other:?}"),
        }
    }

    #[test]
    fn multi_lock_refuses() {
        let expr = apex_expr();
        let req = LockRequest {
            expr: &expr,
            locked_value: Quantity::mm(2255.0),
            free_driver: "span",
            env: env(&[("heel", Quantity::mm(90.0)), ("pitch", Quantity::deg(30.0))]),
            spec: SolveSpec {
                free_unit: Unit::Mm,
                bounds: None,
                monotonic: None,
                tolerance: 0.5,
            },
            extra_locks: 1, // <- two locks
            extra_free: 0,
        };
        match solve_lock(&req) {
            LockOutcome::Refused { note, finding } => {
                assert!(note.needs_general_solver);
                assert!(finding.contains("multiple"));
            }
            other => panic!("expected refusal, got {other:?}"),
        }
    }

    #[test]
    fn numeric_solve_is_deterministic() {
        let expr = apex_expr();
        let mk = || LockRequest {
            expr: &expr,
            locked_value: Quantity::mm(2255.0),
            free_driver: "pitch",
            env: env(&[("heel", Quantity::mm(90.0)), ("span", Quantity::mm(7500.0))]),
            spec: SolveSpec {
                free_unit: Unit::Deg,
                bounds: Some((1.0, 80.0)),
                monotonic: Some(Monotonicity::Increasing),
                tolerance: 0.05,
            },
            extra_locks: 0,
            extra_free: 0,
        };
        assert_eq!(solve_lock(&mk()), solve_lock(&mk()), "deterministic");
    }
}

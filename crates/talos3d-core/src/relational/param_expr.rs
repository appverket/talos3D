//! Bounded typed scalar expression layer for parametric derivations (PP-RPS-2).
//!
//! Per `RELATIONAL_PARAMETRIC_SUBSTRATE_AGREEMENT.md` ("Parameters And
//! Expressions"): a small, typed scalar layer — arithmetic, units, comparisons,
//! trig, clamps, simple conditionals — **not** an unbounded expression
//! language. Geometry construction stays in explicit `AuthoringScript` steps;
//! this layer only computes scalar derived values (apex height, sash width, …).
//!
//! Units are part of the value type ([`Quantity`]); unit mismatch is a typed
//! error, never a panic. Expressions are deterministic and serializable, expose
//! their parameter dependencies as graph edges, and produce a stable cache key.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::graph::NodeId;

/// Physical unit of a scalar value. Minimal set for parametric framing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    /// Length in millimetres (ADR-004 base unit).
    Mm,
    /// Angle in degrees.
    Deg,
    /// Pure number.
    Dimensionless,
}

/// A scalar value carrying its unit. f64 semantics per ADR-020.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct Quantity {
    pub value: f64,
    pub unit: Unit,
}

impl Quantity {
    pub fn mm(v: f64) -> Self {
        Quantity { value: v, unit: Unit::Mm }
    }
    pub fn deg(v: f64) -> Self {
        Quantity { value: v, unit: Unit::Deg }
    }
    pub fn num(v: f64) -> Self {
        Quantity {
            value: v,
            unit: Unit::Dimensionless,
        }
    }
}

/// Evaluation error. No expression evaluation ever panics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalError {
    /// Operands have incompatible units for the operation.
    UnitMismatch {
        op: &'static str,
        lhs: Unit,
        rhs: Unit,
    },
    /// A required unit was not satisfied (e.g. trig needs an angle).
    ExpectedUnit {
        op: &'static str,
        got: Unit,
        want: Unit,
    },
    /// Referenced parameter is not in the environment.
    MissingParam(String),
    /// Division by zero.
    DivByZero,
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnitMismatch { op, lhs, rhs } => {
                write!(f, "unit mismatch in {op}: {lhs:?} vs {rhs:?}")
            }
            Self::ExpectedUnit { op, got, want } => {
                write!(f, "{op} expected {want:?}, got {got:?}")
            }
            Self::MissingParam(n) => write!(f, "missing parameter '{n}'"),
            Self::DivByZero => write!(f, "division by zero"),
        }
    }
}

impl std::error::Error for EvalError {}

// --- unit-checked arithmetic ------------------------------------------------

fn add(a: Quantity, b: Quantity) -> Result<Quantity, EvalError> {
    if a.unit != b.unit {
        return Err(EvalError::UnitMismatch {
            op: "add",
            lhs: a.unit,
            rhs: b.unit,
        });
    }
    Ok(Quantity {
        value: a.value + b.value,
        unit: a.unit,
    })
}

fn sub(a: Quantity, b: Quantity) -> Result<Quantity, EvalError> {
    if a.unit != b.unit {
        return Err(EvalError::UnitMismatch {
            op: "sub",
            lhs: a.unit,
            rhs: b.unit,
        });
    }
    Ok(Quantity {
        value: a.value - b.value,
        unit: a.unit,
    })
}

fn mul(a: Quantity, b: Quantity) -> Result<Quantity, EvalError> {
    let unit = match (a.unit, b.unit) {
        (u, Unit::Dimensionless) => u,
        (Unit::Dimensionless, u) => u,
        // length*length (area) etc. are not modelled in this bounded layer.
        (lhs, rhs) => {
            return Err(EvalError::UnitMismatch {
                op: "mul",
                lhs,
                rhs,
            })
        }
    };
    Ok(Quantity {
        value: a.value * b.value,
        unit,
    })
}

fn div(a: Quantity, b: Quantity) -> Result<Quantity, EvalError> {
    if b.value == 0.0 {
        return Err(EvalError::DivByZero);
    }
    let unit = match (a.unit, b.unit) {
        (u, Unit::Dimensionless) => u,
        (x, y) if x == y => Unit::Dimensionless, // mm/mm, deg/deg -> number
        (lhs, rhs) => {
            return Err(EvalError::UnitMismatch {
                op: "div",
                lhs,
                rhs,
            })
        }
    };
    Ok(Quantity {
        value: a.value / b.value,
        unit,
    })
}

fn require_angle(q: Quantity, op: &'static str) -> Result<f64, EvalError> {
    match q.unit {
        Unit::Deg => Ok(q.value),
        got => Err(EvalError::ExpectedUnit {
            op,
            got,
            want: Unit::Deg,
        }),
    }
}

/// Comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CmpOp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

/// A bounded scalar expression.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ScalarExpr {
    Lit { q: Quantity },
    Param { name: String },
    Add { lhs: Box<ScalarExpr>, rhs: Box<ScalarExpr> },
    Sub { lhs: Box<ScalarExpr>, rhs: Box<ScalarExpr> },
    Mul { lhs: Box<ScalarExpr>, rhs: Box<ScalarExpr> },
    Div { lhs: Box<ScalarExpr>, rhs: Box<ScalarExpr> },
    Neg { expr: Box<ScalarExpr> },
    /// Trig on an angle (deg) -> dimensionless.
    Sin { expr: Box<ScalarExpr> },
    Cos { expr: Box<ScalarExpr> },
    Tan { expr: Box<ScalarExpr> },
    /// atan2(y, x) -> degrees. Operands must share a unit.
    Atan2 { y: Box<ScalarExpr>, x: Box<ScalarExpr> },
    Min { lhs: Box<ScalarExpr>, rhs: Box<ScalarExpr> },
    Max { lhs: Box<ScalarExpr>, rhs: Box<ScalarExpr> },
    Clamp {
        expr: Box<ScalarExpr>,
        lo: Box<ScalarExpr>,
        hi: Box<ScalarExpr>,
    },
    /// Conditional: if `cond` then `then` else `els`.
    If {
        cond: Box<Predicate>,
        then: Box<ScalarExpr>,
        els: Box<ScalarExpr>,
    },
}

/// A boolean predicate over scalar expressions (guards / validation).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(tag = "pred", rename_all = "snake_case")]
pub enum Predicate {
    Bool { value: bool },
    Cmp {
        op: CmpOp,
        lhs: Box<ScalarExpr>,
        rhs: Box<ScalarExpr>,
    },
    And { children: Vec<Predicate> },
    Or { children: Vec<Predicate> },
    Not { child: Box<Predicate> },
}

pub type Env = BTreeMap<String, Quantity>;

impl ScalarExpr {
    pub fn lit(q: Quantity) -> Self {
        ScalarExpr::Lit { q }
    }
    pub fn param(name: impl Into<String>) -> Self {
        ScalarExpr::Param { name: name.into() }
    }

    /// Evaluate against an environment, unit-checked.
    pub fn eval(&self, env: &Env) -> Result<Quantity, EvalError> {
        match self {
            ScalarExpr::Lit { q } => Ok(*q),
            ScalarExpr::Param { name } => env
                .get(name)
                .copied()
                .ok_or_else(|| EvalError::MissingParam(name.clone())),
            ScalarExpr::Add { lhs, rhs } => add(lhs.eval(env)?, rhs.eval(env)?),
            ScalarExpr::Sub { lhs, rhs } => sub(lhs.eval(env)?, rhs.eval(env)?),
            ScalarExpr::Mul { lhs, rhs } => mul(lhs.eval(env)?, rhs.eval(env)?),
            ScalarExpr::Div { lhs, rhs } => div(lhs.eval(env)?, rhs.eval(env)?),
            ScalarExpr::Neg { expr } => {
                let q = expr.eval(env)?;
                Ok(Quantity {
                    value: -q.value,
                    unit: q.unit,
                })
            }
            ScalarExpr::Sin { expr } => {
                let a = require_angle(expr.eval(env)?, "sin")?;
                Ok(Quantity::num(a.to_radians().sin()))
            }
            ScalarExpr::Cos { expr } => {
                let a = require_angle(expr.eval(env)?, "cos")?;
                Ok(Quantity::num(a.to_radians().cos()))
            }
            ScalarExpr::Tan { expr } => {
                let a = require_angle(expr.eval(env)?, "tan")?;
                Ok(Quantity::num(a.to_radians().tan()))
            }
            ScalarExpr::Atan2 { y, x } => {
                let yq = y.eval(env)?;
                let xq = x.eval(env)?;
                if yq.unit != xq.unit {
                    return Err(EvalError::UnitMismatch {
                        op: "atan2",
                        lhs: yq.unit,
                        rhs: xq.unit,
                    });
                }
                Ok(Quantity::deg(yq.value.atan2(xq.value).to_degrees()))
            }
            ScalarExpr::Min { lhs, rhs } => {
                let a = lhs.eval(env)?;
                let b = rhs.eval(env)?;
                if a.unit != b.unit {
                    return Err(EvalError::UnitMismatch {
                        op: "min",
                        lhs: a.unit,
                        rhs: b.unit,
                    });
                }
                Ok(Quantity {
                    value: a.value.min(b.value),
                    unit: a.unit,
                })
            }
            ScalarExpr::Max { lhs, rhs } => {
                let a = lhs.eval(env)?;
                let b = rhs.eval(env)?;
                if a.unit != b.unit {
                    return Err(EvalError::UnitMismatch {
                        op: "max",
                        lhs: a.unit,
                        rhs: b.unit,
                    });
                }
                Ok(Quantity {
                    value: a.value.max(b.value),
                    unit: a.unit,
                })
            }
            ScalarExpr::Clamp { expr, lo, hi } => {
                let v = expr.eval(env)?;
                let l = lo.eval(env)?;
                let h = hi.eval(env)?;
                if v.unit != l.unit || v.unit != h.unit {
                    return Err(EvalError::UnitMismatch {
                        op: "clamp",
                        lhs: v.unit,
                        rhs: l.unit,
                    });
                }
                Ok(Quantity {
                    value: v.value.clamp(l.value, h.value),
                    unit: v.unit,
                })
            }
            ScalarExpr::If { cond, then, els } => {
                if cond.eval(env)? {
                    then.eval(env)
                } else {
                    els.eval(env)
                }
            }
        }
    }

    /// Parameter names this expression reads.
    pub fn dependencies(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        self.collect_deps(&mut out);
        out
    }

    fn collect_deps(&self, out: &mut BTreeSet<String>) {
        match self {
            ScalarExpr::Lit { .. } => {}
            ScalarExpr::Param { name } => {
                out.insert(name.clone());
            }
            ScalarExpr::Neg { expr }
            | ScalarExpr::Sin { expr }
            | ScalarExpr::Cos { expr }
            | ScalarExpr::Tan { expr } => expr.collect_deps(out),
            ScalarExpr::Add { lhs, rhs }
            | ScalarExpr::Sub { lhs, rhs }
            | ScalarExpr::Mul { lhs, rhs }
            | ScalarExpr::Div { lhs, rhs }
            | ScalarExpr::Min { lhs, rhs }
            | ScalarExpr::Max { lhs, rhs } => {
                lhs.collect_deps(out);
                rhs.collect_deps(out);
            }
            ScalarExpr::Atan2 { y, x } => {
                y.collect_deps(out);
                x.collect_deps(out);
            }
            ScalarExpr::Clamp { expr, lo, hi } => {
                expr.collect_deps(out);
                lo.collect_deps(out);
                hi.collect_deps(out);
            }
            ScalarExpr::If { cond, then, els } => {
                cond.collect_deps(out);
                then.collect_deps(out);
                els.collect_deps(out);
            }
        }
    }

    /// Dependencies as graph edges (each param name -> a `ComponentParam` node
    /// in the given component). A thin adapter — no second graph.
    pub fn dependency_nodes(&self, component: u64) -> Vec<NodeId> {
        self.dependencies()
            .into_iter()
            .map(|name| NodeId::param(component, name))
            .collect()
    }

    /// Stable cache key for `(expr, resolved env)` — equal inputs ⇒ equal key.
    pub fn cache_key(&self, env: &Env) -> String {
        let mut hasher = blake3::Hasher::new();
        // expression structure
        hasher.update(serde_json::to_string(self).unwrap_or_default().as_bytes());
        hasher.update(b"|");
        // only the params this expr actually reads, in sorted order
        for name in self.dependencies() {
            hasher.update(name.as_bytes());
            hasher.update(b"=");
            if let Some(q) = env.get(&name) {
                hasher.update(format!("{:?}:{}", q.unit, q.value).as_bytes());
            } else {
                hasher.update(b"<missing>");
            }
            hasher.update(b";");
        }
        hasher.finalize().to_hex().to_string()
    }
}

impl Predicate {
    pub fn eval(&self, env: &Env) -> Result<bool, EvalError> {
        match self {
            Predicate::Bool { value } => Ok(*value),
            Predicate::Cmp { op, lhs, rhs } => {
                let a = lhs.eval(env)?;
                let b = rhs.eval(env)?;
                if a.unit != b.unit {
                    return Err(EvalError::UnitMismatch {
                        op: "cmp",
                        lhs: a.unit,
                        rhs: b.unit,
                    });
                }
                Ok(match op {
                    CmpOp::Lt => a.value < b.value,
                    CmpOp::Le => a.value <= b.value,
                    CmpOp::Gt => a.value > b.value,
                    CmpOp::Ge => a.value >= b.value,
                    CmpOp::Eq => a.value == b.value,
                    CmpOp::Ne => a.value != b.value,
                })
            }
            Predicate::And { children } => {
                for c in children {
                    if !c.eval(env)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Predicate::Or { children } => {
                for c in children {
                    if c.eval(env)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Predicate::Not { child } => Ok(!child.eval(env)?),
        }
    }

    /// Parameter names this predicate reads.
    pub fn dependencies(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        self.collect_deps(&mut out);
        out
    }

    fn collect_deps(&self, out: &mut BTreeSet<String>) {
        match self {
            Predicate::Bool { .. } => {}
            Predicate::Cmp { lhs, rhs, .. } => {
                lhs.collect_deps(out);
                rhs.collect_deps(out);
            }
            Predicate::And { children } | Predicate::Or { children } => {
                for c in children {
                    c.collect_deps(out);
                }
            }
            Predicate::Not { child } => child.collect_deps(out),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(e: ScalarExpr) -> Box<ScalarExpr> {
        Box::new(e)
    }

    fn env(pairs: &[(&str, Quantity)]) -> Env {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn unit_mismatch_is_typed_error_not_panic() {
        // mm + deg -> error
        let e = ScalarExpr::Add {
            lhs: b(ScalarExpr::lit(Quantity::mm(10.0))),
            rhs: b(ScalarExpr::lit(Quantity::deg(5.0))),
        };
        assert!(matches!(
            e.eval(&Env::new()).unwrap_err(),
            EvalError::UnitMismatch { .. }
        ));
        // tan of a length -> error
        let t = ScalarExpr::Tan {
            expr: b(ScalarExpr::lit(Quantity::mm(3.0))),
        };
        assert!(matches!(
            t.eval(&Env::new()).unwrap_err(),
            EvalError::ExpectedUnit { .. }
        ));
    }

    #[test]
    fn truss_apex_math() {
        // apex = heel + (span/2) * tan(pitch)
        let apex = ScalarExpr::Add {
            lhs: b(ScalarExpr::param("heel")),
            rhs: b(ScalarExpr::Mul {
                lhs: b(ScalarExpr::Div {
                    lhs: b(ScalarExpr::param("span")),
                    rhs: b(ScalarExpr::lit(Quantity::num(2.0))),
                }),
                rhs: b(ScalarExpr::Tan {
                    expr: b(ScalarExpr::param("pitch")),
                }),
            }),
        };
        let e = env(&[
            ("heel", Quantity::mm(90.0)),
            ("span", Quantity::mm(7500.0)),
            ("pitch", Quantity::deg(30.0)),
        ]);
        let q = apex.eval(&e).unwrap();
        assert_eq!(q.unit, Unit::Mm);
        // 90 + 3750*tan(30) = 90 + 3750*0.57735 = 2255.0...
        assert!((q.value - 2255.0).abs() < 1.0, "apex ~2255mm, got {}", q.value);
        // dependencies are span, pitch, heel
        assert_eq!(
            apex.dependencies(),
            ["heel", "pitch", "span"].iter().map(|s| s.to_string()).collect()
        );
        assert_eq!(apex.dependency_nodes(1).len(), 3);
    }

    #[test]
    fn window_sash_split_math() {
        // sash = (width - 2*frame - mullion) / 2
        let sash = ScalarExpr::Div {
            lhs: b(ScalarExpr::Sub {
                lhs: b(ScalarExpr::Sub {
                    lhs: b(ScalarExpr::param("width")),
                    rhs: b(ScalarExpr::Mul {
                        lhs: b(ScalarExpr::lit(Quantity::num(2.0))),
                        rhs: b(ScalarExpr::param("frame")),
                    }),
                }),
                rhs: b(ScalarExpr::param("mullion")),
            }),
            rhs: b(ScalarExpr::lit(Quantity::num(2.0))),
        };
        let e = env(&[
            ("width", Quantity::mm(1500.0)),
            ("frame", Quantity::mm(58.0)),
            ("mullion", Quantity::mm(68.0)),
        ]);
        let q = sash.eval(&e).unwrap();
        assert_eq!(q.unit, Unit::Mm);
        // (1500 - 116 - 68)/2 = 1316/2 = 658
        assert_eq!(q.value, 658.0);
    }

    #[test]
    fn predicate_guard() {
        // width >= min_width
        let guard = Predicate::Cmp {
            op: CmpOp::Ge,
            lhs: b(ScalarExpr::param("width")),
            rhs: b(ScalarExpr::param("min_width")),
        };
        let pass = env(&[("width", Quantity::mm(1200.0)), ("min_width", Quantity::mm(600.0))]);
        let fail = env(&[("width", Quantity::mm(400.0)), ("min_width", Quantity::mm(600.0))]);
        assert!(guard.eval(&pass).unwrap());
        assert!(!guard.eval(&fail).unwrap());
        // mismatched units rejected
        let bad = env(&[("width", Quantity::mm(1200.0)), ("min_width", Quantity::deg(600.0))]);
        assert!(guard.eval(&bad).is_err());
    }

    #[test]
    fn conditional_select() {
        // if width >= 1000 then width else 1000   (clamp-ish)
        let e = ScalarExpr::If {
            cond: Box::new(Predicate::Cmp {
                op: CmpOp::Ge,
                lhs: b(ScalarExpr::param("width")),
                rhs: b(ScalarExpr::lit(Quantity::mm(1000.0))),
            }),
            then: b(ScalarExpr::param("width")),
            els: b(ScalarExpr::lit(Quantity::mm(1000.0))),
        };
        assert_eq!(e.eval(&env(&[("width", Quantity::mm(1500.0))])).unwrap().value, 1500.0);
        assert_eq!(e.eval(&env(&[("width", Quantity::mm(800.0))])).unwrap().value, 1000.0);
    }

    #[test]
    fn cache_key_normalization() {
        let e = ScalarExpr::Add {
            lhs: b(ScalarExpr::param("a")),
            rhs: b(ScalarExpr::param("b")),
        };
        let k1 = e.cache_key(&env(&[("a", Quantity::mm(1.0)), ("b", Quantity::mm(2.0))]));
        let k2 = e.cache_key(&env(&[("a", Quantity::mm(1.0)), ("b", Quantity::mm(2.0))]));
        let k3 = e.cache_key(&env(&[("a", Quantity::mm(1.0)), ("b", Quantity::mm(3.0))]));
        assert_eq!(k1, k2, "equal inputs => equal key");
        assert_ne!(k1, k3, "different inputs => different key");
        // irrelevant params don't affect the key
        let k4 = e.cache_key(&env(&[
            ("a", Quantity::mm(1.0)),
            ("b", Quantity::mm(2.0)),
            ("unused", Quantity::mm(99.0)),
        ]));
        assert_eq!(k1, k4);
    }

    #[test]
    fn serde_round_trip() {
        let e = ScalarExpr::Mul {
            lhs: b(ScalarExpr::param("span")),
            rhs: b(ScalarExpr::lit(Quantity::num(0.5))),
        };
        let s = serde_json::to_string(&e).unwrap();
        let e2: ScalarExpr = serde_json::from_str(&s).unwrap();
        assert_eq!(e, e2);
    }
}

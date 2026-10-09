//! Exact symbolic execution of straight-line code on polynomials.
//!
//! Cell values are polynomials with integer coefficients in variables for
//! cell values and for floors of polynomials divided by 256. Every byte
//! operation is exact on these: wrapping is subtracting 256 times the floor,
//! and a comparison of bytes `a < b` is the floor of `(b - a + 255) / 256`.
//! Floors are kept in a canonical form, so carries computed by different
//! nodes from the same sum are the same variable and cancel out. This proves
//! things decision diagrams can't, like a schoolbook multiplication computing
//! a product.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap};

use super::{AstNode, Operand};

/// A product of variables, in order.
type Monomial = Vec<u32>;

/// A polynomial, without zero coefficients.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Poly(BTreeMap<Monomial, i128>);

impl Poly {
    pub fn constant(value: i128) -> Self {
        let mut poly = Self::default();
        poly.0.insert(Vec::new(), value);
        poly.0.retain(|_, &mut coefficient| coefficient != 0);
        poly
    }

    fn var(var: u32) -> Self {
        Self(BTreeMap::from([(vec![var], 1)]))
    }

    fn add_term(&mut self, monomial: Monomial, coefficient: i128) -> Option<()> {
        match self.0.entry(monomial) {
            Entry::Occupied(mut entry) => {
                let sum = entry.get().checked_add(coefficient)?;
                if sum == 0 {
                    entry.remove();
                } else {
                    *entry.get_mut() = sum;
                }
            }
            Entry::Vacant(entry) => {
                if coefficient != 0 {
                    entry.insert(coefficient);
                }
            }
        }
        Some(())
    }

    pub fn add(&self, other: &Self) -> Option<Self> {
        let mut sum = self.clone();
        for (monomial, &coefficient) in &other.0 {
            sum.add_term(monomial.clone(), coefficient)?;
        }
        Some(sum)
    }

    pub fn scale(&self, factor: i128) -> Option<Self> {
        let mut scaled = Self::default();
        for (monomial, &coefficient) in &self.0 {
            scaled.add_term(monomial.clone(), coefficient.checked_mul(factor)?)?;
        }
        Some(scaled)
    }

    pub fn sub(&self, other: &Self) -> Option<Self> {
        self.add(&other.scale(-1)?)
    }

    pub fn mul(&self, other: &Self) -> Option<Self> {
        let mut product = Self::default();
        for (a, &x) in &self.0 {
            for (b, &y) in &other.0 {
                let mut monomial = a.clone();
                monomial.extend(b);
                monomial.sort_unstable();
                product.add_term(monomial, x.checked_mul(y)?)?;
            }
            if product.0.len() > MAX_TERMS {
                return None;
            }
        }
        Some(product)
    }

    /// Whether the polynomial is a multiple of `modulus` for all values.
    pub fn is_multiple_of(&self, modulus: i128) -> bool {
        self.0
            .values()
            .all(|coefficient| coefficient % modulus == 0)
    }
}

/// Floors substituted by their arguments when bounding a polynomial.
const MAX_SUBSTITUTIONS: usize = 8;
/// Proofs are given up when polynomials get more terms than this.
const MAX_TERMS: usize = 256;

/// Variables and what's known about them.
#[derive(Default)]
pub struct Algebra {
    /// The smallest and largest value of each variable
    ranges: Vec<(i128, i128)>,
    /// The floor variables, by the polynomial divided by 256
    floors: HashMap<Poly, u32>,
    /// The polynomial each floor variable is of, divided by 256
    arguments: HashMap<u32, Poly>,
}

impl Algebra {
    /// A new variable holding a byte.
    pub fn byte(&mut self) -> Poly {
        self.var((0, 255))
    }

    fn var(&mut self, range: (i128, i128)) -> Poly {
        let var = u32::try_from(self.ranges.len()).unwrap();
        self.ranges.push(range);
        Poly::var(var)
    }

    /// The smallest and largest value of a polynomial, or a range including
    /// them. Floors are first replaced by their arguments minus a remainder
    /// from 0 to 255, over 256, newest first, so that floors of related
    /// numbers cancel out.
    fn range(&self, poly: &Poly) -> Option<(i128, i128)> {
        // Floors can only cancel out with others.
        let floors: std::collections::BTreeSet<u32> = poly
            .0
            .keys()
            .flatten()
            .copied()
            .filter(|var| self.arguments.contains_key(var))
            .collect();
        if floors.len() < 2 {
            return self.bounds(poly);
        }
        let mut poly = poly.clone();
        let mut denominator = 1i128;
        // Remainder variables are numbered after all others.
        let mut remainder = u32::try_from(self.ranges.len()).ok()?;
        for _ in 0..MAX_SUBSTITUTIONS {
            let Some(var) = poly
                .0
                .keys()
                .flatten()
                .copied()
                .filter(|var| self.arguments.contains_key(var))
                .max()
            else {
                break;
            };
            // Only floors appearing linearly are replaced.
            if poly
                .0
                .keys()
                .any(|monomial| monomial.iter().filter(|&&v| v == var).count() > 1)
            {
                break;
            }
            let Some(substituted) = self
                .substitute(&poly, var, remainder)
                .filter(|poly| poly.0.len() <= MAX_TERMS)
            else {
                break;
            };
            let Some(scaled) = denominator.checked_mul(256) else {
                break;
            };
            poly = substituted;
            denominator = scaled;
            remainder += 1;
        }

        let (low, high) = self.bounds(&poly)?;
        // The polynomial's value is an integer.
        Some((
            -(-low).div_euclid(denominator),
            high.div_euclid(denominator),
        ))
    }

    /// 256 times a polynomial, with a floor variable replaced by its argument
    /// minus a remainder variable.
    fn substitute(&self, poly: &Poly, var: u32, remainder: u32) -> Option<Poly> {
        let replacement = self.arguments[&var].sub(&Poly::var(remainder))?;
        let mut result = Poly::default();
        for (monomial, &coefficient) in &poly.0 {
            if let Some(index) = monomial.iter().position(|&v| v == var) {
                let mut rest = monomial.clone();
                rest.remove(index);
                let term = Poly(BTreeMap::from([(rest, coefficient)]));
                result = result.add(&term.mul(&replacement)?)?;
            } else {
                result.add_term(monomial.clone(), coefficient.checked_mul(256)?)?;
            }
        }
        Some(result)
    }

    /// Bounds of a polynomial from the ranges of its variables. Variables
    /// numbered after all others are remainders from 0 to 255.
    fn bounds(&self, poly: &Poly) -> Option<(i128, i128)> {
        let (mut low, mut high) = (0i128, 0i128);
        for (monomial, &coefficient) in &poly.0 {
            let (mut a, mut b) = (coefficient, coefficient);
            for &var in monomial {
                let (c, d) = self.ranges.get(var as usize).copied().unwrap_or((0, 255));
                let products = [
                    a.checked_mul(c)?,
                    a.checked_mul(d)?,
                    b.checked_mul(c)?,
                    b.checked_mul(d)?,
                ];
                a = *products.iter().min()?;
                b = *products.iter().max()?;
            }
            low = low.checked_add(a)?;
            high = high.checked_add(b)?;
        }
        Some((low, high))
    }

    /// The floor of a polynomial divided by 256. Multiples of 256 are taken
    /// out of the coefficients first, leaving them between -127 and 128, so
    /// what's left is canonical.
    pub fn floor(&mut self, poly: &Poly) -> Option<Poly> {
        let mut quotient = Poly::default();
        let mut rest = Poly::default();
        for (monomial, &coefficient) in &poly.0 {
            // Taking -1 times a variable out as -256 + 255 would make ones
            // complements look nothing like the numbers they complement.
            let mut remainder = coefficient.rem_euclid(256);
            if remainder > 128 {
                remainder -= 256;
            }
            quotient.add_term(monomial.clone(), (coefficient - remainder) / 256)?;
            rest.add_term(monomial.clone(), remainder)?;
        }
        let (low, high) = self.range(&rest)?;
        let range = (low.div_euclid(256), high.div_euclid(256));
        // Floors of negated numbers are negated floors, so the same carry
        // computed as a comparison either way round comes out the same:
        // floor(r / 256) = -floor((255 - r) / 256).
        let negative = rest
            .0
            .iter()
            .find(|(monomial, _)| !monomial.is_empty())
            .is_some_and(|(_, &coefficient)| coefficient < 0);
        let floor = if range.0 == range.1 {
            Poly::constant(range.0)
        } else if negative {
            self.floor(&Poly::constant(255).sub(&rest)?)?.scale(-1)?
        } else if let Some(&var) = self.floors.get(&rest) {
            Poly::var(var)
        } else {
            let floor = self.var(range);
            let var = *floor.0.keys().next()?.first()?;
            self.arguments.insert(var, rest.clone());
            self.floors.insert(rest, var);
            floor
        };
        quotient.add(&floor)
    }

    /// A polynomial wrapped to a byte.
    pub fn wrap(&mut self, poly: &Poly) -> Option<Poly> {
        poly.sub(&self.floor(poly)?.scale(256)?)
    }

    fn operand(&mut self, cells: &BTreeMap<i32, Poly>, operand: Operand) -> Option<Poly> {
        match operand.reads() {
            Some(cell) => {
                let value = cells
                    .get(&cell)?
                    .scale(i128::from(operand.scale))?
                    .add(&Poly::constant(i128::from(operand.bias)))?;
                self.wrap(&value)
            }
            None => Some(Poly::constant(i128::from(operand.bias))),
        }
    }

    /// Run straight-line nodes other than `Word` on cell values.
    pub fn run(&mut self, nodes: &[AstNode], cells: &mut BTreeMap<i32, Poly>) -> Option<()> {
        for node in nodes {
            let (dst, addend) = match *node {
                AstNode::Add(offset, value) => (offset, Poly::constant(i128::from(value))),
                AstNode::Set(offset, value) => {
                    cells.insert(offset, Poly::constant(i128::from(value)));
                    continue;
                }
                AstNode::MulAdd { src, dst, factor } => {
                    (dst, cells.get(&src)?.scale(i128::from(factor))?)
                }
                AstNode::CondAdd {
                    lhs,
                    rhs,
                    dst,
                    value,
                } => {
                    let lhs = self.operand(cells, lhs)?;
                    let rhs = self.operand(cells, rhs)?;
                    // lhs < rhs <=> rhs - lhs + 255 >= 256
                    let less = self.floor(&rhs.sub(&lhs)?.add(&Poly::constant(255))?)?;
                    (dst, less.scale(i128::from(value))?)
                }
                AstNode::ProductAdd {
                    base,
                    step,
                    count,
                    high,
                    dst,
                    value,
                } => {
                    let step = self.operand(cells, step)?;
                    let count = self.operand(cells, count)?;
                    let total = self.operand(cells, base)?.add(&step.mul(&count)?)?;
                    let byte = if high {
                        self.floor(&total)?
                    } else {
                        self.wrap(&total)?
                    };
                    (dst, byte.scale(i128::from(value))?)
                }
                _ => return None,
            };
            let sum = cells.get(&dst)?.add(&addend)?;
            let wrapped = self.wrap(&sum)?;
            cells.insert(dst, wrapped);
        }
        Some(())
    }
}

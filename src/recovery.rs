use dusk_plonk::fft::EvaluationDomain;
use dusk_plonk::prelude::BlsScalar;

pub fn reconstruct_poly(
    // domain I'm working with
    // all (i)ffts to be performed on it
    eval_domain: EvaluationDomain,
    // subset of avialable data
    subset: Vec<Option<BlsScalar>>,
) -> Vec<BlsScalar> {
    let mut missing_indices = Vec::new();
    for i in 0..subset.len() {
        if let None = subset[i] {
            missing_indices.push(i as u64);
        }
    }
    let (mut zero_poly, zero_eval) =
        zero_poly_fn(eval_domain, &missing_indices[..], subset.len() as u64);
    for i in 0..subset.len() {
        if let None = subset[i] {
            assert_eq!(
                zero_eval[i],
                BlsScalar::zero(),
                "bad zero poly evaluation !"
            );
        }
    }
    let mut poly_evals_with_zero: Vec<BlsScalar> = Vec::new();
    for i in 0..subset.len() {
        if let Some(v) = subset[i] {
            poly_evals_with_zero.push(v * zero_eval[i]);
        } else {
            poly_evals_with_zero.push(BlsScalar::zero());
        }
    }
    let mut poly_with_zero = eval_domain.ifft(&poly_evals_with_zero[..]);
    shift_poly(&mut poly_with_zero[..]);
    shift_poly(&mut zero_poly[..]);
    let mut eval_shifted_poly_with_zero = eval_domain.fft(&poly_with_zero[..]);
    let eval_shifted_zero_poly = eval_domain.fft(&zero_poly[..]);
    for i in 0..eval_shifted_poly_with_zero.len() {
        eval_shifted_poly_with_zero[i] *= eval_shifted_zero_poly[i].invert().unwrap();
    }
    let mut shifted_reconstructed_poly = eval_domain.ifft(&eval_shifted_poly_with_zero[..]);
    unshift_poly(&mut shifted_reconstructed_poly[..]);
    let reconstructed_data = eval_domain.fft(&shifted_reconstructed_poly[..]);
    for i in 0..subset.len() {
        if let Some(v) = subset[i] {
            assert_eq!(
                v, reconstructed_data[i],
                "failed to reconstruct correctly !"
            )
        }
    }
    reconstructed_data
}

fn expand_root_of_unity(eval_domain: EvaluationDomain) -> Vec<BlsScalar> {
    let root_of_unity = eval_domain.group_gen;
    let mut roots: Vec<BlsScalar> = Vec::new();
    roots.push(BlsScalar::one());
    roots.push(root_of_unity);
    let mut i = 1;
    while roots[i] != BlsScalar::one() {
        roots.push(roots[i] * root_of_unity);
        i += 1;
    }
    return roots;
}

fn zero_poly_fn(
    eval_domain: EvaluationDomain,
    missing_indices: &[u64],
    length: u64,
) -> (Vec<BlsScalar>, Vec<BlsScalar>) {
    let expanded_r_o_u = expand_root_of_unity(eval_domain);
    let domain_stride = eval_domain.size() as u64 / length;
    let mut zero_poly: Vec<BlsScalar> = Vec::with_capacity(length as usize);
    let mut sub: BlsScalar;
    for i in 0..missing_indices.len() {
        let v = missing_indices[i as usize];
        sub = BlsScalar::zero() - expanded_r_o_u[(v * domain_stride) as usize];
        zero_poly.push(sub);
        if i > 0 {
            zero_poly[i] = zero_poly[i] + zero_poly[i - 1];
            for j in (1..i).rev() {
                zero_poly[j] = zero_poly[j] * sub;
                zero_poly[j] = zero_poly[j] + zero_poly[j - 1];
            }
            zero_poly[0] = zero_poly[0] * sub
        }
    }
    zero_poly.push(BlsScalar::one());
    for _ in zero_poly.len()..zero_poly.capacity() {
        zero_poly.push(BlsScalar::zero());
    }
    let zero_eval = eval_domain.fft(&zero_poly[..]);
    (zero_poly, zero_eval)
}

// in-place shifting
fn shift_poly(poly: &mut [BlsScalar]) {
    // primitive root of unity
    let shift_factor = BlsScalar::from(5);
    let mut factor_power = BlsScalar::one();
    // hoping it won't panic, though it should be handled properly
    //
    // this is actually 1/ shift_factor --- multiplicative inverse
    let inv_factor = shift_factor.invert().unwrap();

    for i in 0..poly.len() {
        poly[i] *= factor_power;
        factor_power *= inv_factor;
    }
}

// in-place unshifting
fn unshift_poly(poly: &mut [BlsScalar]) {
    // primitive root of unity
    let shift_factor = BlsScalar::from(5);
    let mut factor_power = BlsScalar::one();

    for i in 0..poly.len() {
        poly[i] *= factor_power;
        factor_power *= shift_factor;
    }
}

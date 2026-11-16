macro_rules! clmm_fail {
    ($id:expr, $reason:expr) => {{
        warn!(
            target: "quote",
            pool_id = $id,
            reason = $reason,
            "CLMM quote failed"
        );
        return None;
    }};
}

macro_rules! cp_fail {
    ($id:expr, $reason:expr) => {{
        warn!(
            target: "quote",
            pool_id = $id,
            reason = $reason,
            "CP quote failed"
        );
        return None;
    }};
}

use primitive_types::U256;
use tracing::warn;
// use tracing::{info, panic};

pub fn quote_cp(
    dx: u64,
    x_vault: u64,
    y_vault: u64,
    fee_bps: u64,
    a_to_b: bool,
    _a_is_base: bool,
    pool_id: u32, // debug
) -> Option<u64> {
    if dx == 0 {
        cp_fail!(pool_id, "zero dx");
    }
    if x_vault == 0 {
        cp_fail!(pool_id, "x vault empty");
    }
    if y_vault == 0 {
        cp_fail!(pool_id, "y vault empty");
    }
    if fee_bps >= 10_000 {
        cp_fail!(pool_id, "fee to large");
    }

    // Apply fee.
    let dx_f = dx.checked_mul(10_000 - fee_bps)? / 10_000;

    if dx_f == 0 {
        cp_fail!(pool_id, "zero dx after fee");
    }

    // dy = y * dx / (x + dx) // dy mean tokens recieved for dx (price lowers)
    // dx = x * dy / (y + dy) // dx mean tokens recieved for dy (price rises)

    let num;
    let den;

    // price inc/dec as WSOL/USDC (x/y)
    if a_to_b {
        // decreases price

        // so dx (input sol) decrease price of WSOL

        num = y_vault.checked_mul(dx_f)?;
        den = x_vault.checked_add(dx_f)?;
    } else {
        // increases price

        // so dy (input usdc) increase price of WSOL

        num = x_vault.checked_mul(dx_f)?;
        den = y_vault.checked_add(dx_f)?;
    }

    Some(num / den)
}

pub fn quote_clmm(
    dx: u64,
    tick: i32, // current tick
    spacing: u16,
    sqrt_p: u128,
    liquidity: u128,
    start_idx: i32,          // start tick index of the *current* array
    tick_array: &[i128; 88], // current
    tick_array_lo: &[i128; 176],
    tick_array_hi: &[i128; 176],
    fee_bps: u16,
    a_to_b: bool,
    _a_is_base: bool, // kept for caller; not used inside math
    pool_id: u32,     // debug
) -> Option<u64> {
    if dx == 0 {
        clmm_fail!(pool_id, "zero input");
    }

    if sqrt_p == 0 {
        clmm_fail!(pool_id, "zero sqrt price");
    }

    if liquidity == 0 {
        clmm_fail!(pool_id, "zero liquidity");
    }

    if fee_bps >= 10_000 {
        clmm_fail!(pool_id, "invalid fee");
    }

    // fee
    let dx = U256::from(dx.checked_mul(10_000 - fee_bps as u64)? / 10_000);
    if dx == U256::zero() {
        clmm_fail!(pool_id, "zero input after fee");
    }

    let q = U256::one() << 64;
    let mut remaining = dx;
    let mut dy = U256::zero();
    let mut s = U256::from(sqrt_p);
    let mut l = U256::from(liquidity);
    let mut current_tick = tick;

    // fast – completely empty arrays → pure virtual liquidity
    if tick_array.iter().all(|&x| x == 0)
        && tick_array_lo.iter().all(|&x| x == 0)
        && tick_array_hi.iter().all(|&x| x == 0)
    {
        let amount_out = if a_to_b {
            // A → B
            let numerator = l * s * q;
            let denominator = l * q + remaining * s;
            let s_new = numerator / denominator;
            l * (s - s_new) / q
        } else {
            // B → A
            let s_new = s + remaining * q / l;
            let numerator = l * (s_new - s) * q;
            let denominator = s_new * s;
            numerator / denominator
        };
        return Some(u64::try_from(amount_out).ok()?);
    }

    // ------------------------------------------------------------------
    // Main loop – stay inside the three arrays we already own
    // ------------------------------------------------------------------
    while !remaining.is_zero() {
        // located the next initialized tick that is still inside windows
        let (next_tick, net) = if a_to_b {
            let Some(result) = crate::functions::find_prev_initialized(
                current_tick,
                spacing,
                start_idx,
                tick_array,
                tick_array_lo,
            ) else {
                clmm_fail!(pool_id, "find_prev_initialized returned None");
            };
            result
        } else {
            let Some(result) = crate::functions::find_next_initialized(
                current_tick,
                spacing,
                start_idx,
                tick_array,
                tick_array_hi,
            ) else {
                clmm_fail!(pool_id, "find_next_initialized returned None");
            };
            result
        };
        // How far can we move in sqrt-price space before we hit that tick?
        let target_sqrt = crate::functions::tick_to_sqrt_price(next_tick);
        let target_s = U256::from(target_sqrt);

        // this is connected to a_is_base check
        if (a_to_b && target_s > s) || (!a_to_b && target_s < s) {
            clmm_fail!(pool_id, "invalid target sqrt price direction");
        }

        let (amount_in_to_tick, amount_out_to_tick, s_new) = if a_to_b {
            let denom = l * q + remaining * s;
            let s_candidate = (l * s * q) / denom;

            if s_candidate > target_s {
                // exhaust remaining before the tick
                let dy_got = l * (s - s_candidate) / q;
                (remaining, dy_got, s_candidate)
            } else {
                // we reach the tick
                let dx_needed = l * (s - target_s) * q / (s * target_s);
                let dy_got = l * (s - target_s) / q;
                (dx_needed, dy_got, target_s)
            }
        } else {
            let s_candidate = s + remaining * q / l;

            if s_candidate < target_s {
                // exhaust remaining before the tick
                let dy_got = l * (s_candidate - s) * q / (s * s_candidate);
                (remaining, dy_got, s_candidate)
            } else {
                // we reach the tick
                let dx_needed = l * (target_s - s) / q;
                let dy_got = l * (target_s - s) * q / (s * target_s);
                (dx_needed, dy_got, target_s)
            }
        };

        let amount_in_to_tick = amount_in_to_tick.min(remaining);
        remaining -= amount_in_to_tick;
        dy += amount_out_to_tick;
        s = s_new;

        if remaining.is_zero() {
            break;
        }

        // We landed exactly on an initialized tick → update liquidity
        // (net is already the correct signed value from the array)
        if a_to_b {
            // crossing left → subtract net
            l = if net >= 0 {
                l.checked_sub(U256::from(net as u128))?
            } else {
                l + U256::from((-net) as u128)
            };
        } else {
            // crossing right → add net
            l = if net >= 0 {
                l + U256::from(net as u128)
            } else {
                l.checked_sub(U256::from((-net) as u128))?
            };
        }
        current_tick = if a_to_b { next_tick - 1 } else { next_tick };

        // Safety – if liquidity went to zero we cannot continue
        if l.is_zero() {
            clmm_fail!(pool_id, "liquidity exhausted");
        }
    }

    if !remaining.is_zero() {
        clmm_fail!(pool_id, "!remaining.is_zero");
    }

    // info!(" [QUOTE] [{pool_id:?}] {} {}", dx, dy);
    Some(u64::try_from(dy).ok()?)
}

pub fn quote_dlmm(
    _dx: u64,
    _active_id: i32,
    _bin_step: u16,
    _bin_array: [(u64, u64); 70],
    _fee_bps: u64,
    _a_to_b: bool,
) -> Option<u64> {
    Some(0)
}

pub fn quote_ob(
    _dx: u64,
    _mid_price: u64,
    _current_array: [(u64, i64); 32], // 32 max
    _fee_bps: u64,
    _a_to_b: bool,
) -> Option<u64> {
    Some(0)
}

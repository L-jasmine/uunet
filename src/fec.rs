//! Reed-Solomon CM256(GF(256))编解码纯函数,从 omarchyuu transport/rsfec.rs
//! 拷贝;仅保留混合 KCP 的 FEC 依赖(cm256_* + GF 域运算),RTP 分片结构不在此。

use anyhow::{ensure, Result};
use std::sync::OnceLock;

pub(crate) fn cm256_decode_originals(
    k: u8,
    m: u8,
    shard_size: usize,
    selected: &mut [(u8, Vec<u8>)],
) -> Result<Vec<Vec<u8>>> {
    let count = usize::from(k);
    ensure!(
        count != 0 && selected.len() == count,
        "CM256 requires exactly k shards"
    );
    ensure!(u16::from(k) + u16::from(m) <= 256, "invalid CM256 k+m");
    ensure!(
        selected.iter().all(|(_, shard)| shard.len() >= shard_size),
        "short CM256 shard"
    );

    let mut augmented = vec![vec![0_u8; count * 2]; count];
    for (row, (index, _)) in selected.iter().enumerate() {
        ensure!(
            u16::from(*index) < u16::from(k) + u16::from(m),
            "CM256 shard index is outside k+m"
        );
        if *index < k {
            augmented[row][usize::from(*index)] = 1;
        } else {
            for (column, value) in augmented[row][..count].iter_mut().enumerate() {
                *value = matrix_element(*index, k, column as u8);
            }
        }
        augmented[row][count + row] = 1;
    }
    invert_matrix(&mut augmented, count)?;

    let mut originals = vec![vec![0_u8; shard_size]; count];
    for original in 0..count {
        for (received, (_, shard)) in selected.iter().enumerate() {
            multiply_add(
                &mut originals[original],
                &shard[..shard_size],
                augmented[original][count + received],
            );
        }
    }
    Ok(originals)
}

pub(crate) fn cm256_encode_repairs(
    originals: &[Vec<u8>],
    repair_count: u8,
    shard_size: usize,
) -> Result<Vec<Vec<u8>>> {
    let original_count = u8::try_from(originals.len())
        .map_err(|_| anyhow::anyhow!("CM256 original count exceeds 255"))?;
    ensure!(
        original_count != 0,
        "CM256 requires at least one original shard"
    );
    ensure!(
        repair_count != 0,
        "CM256 requires at least one repair shard"
    );
    ensure!(
        u16::from(original_count) + u16::from(repair_count) <= 256,
        "invalid CM256 original+repair count"
    );
    ensure!(
        originals.iter().all(|shard| shard.len() >= shard_size),
        "short CM256 original shard"
    );

    let mut repairs = Vec::with_capacity(usize::from(repair_count));
    for repair in 0..repair_count {
        let row = original_count.wrapping_add(repair);
        let mut output = vec![0_u8; shard_size];
        for (column, shard) in originals.iter().enumerate() {
            multiply_add(
                &mut output,
                &shard[..shard_size],
                matrix_element(row, original_count, column as u8),
            );
        }
        repairs.push(output);
    }
    Ok(repairs)
}

fn invert_matrix(matrix: &mut [Vec<u8>], count: usize) -> Result<()> {
    for column in 0..count {
        let pivot = (column..count)
            .find(|row| matrix[*row][column] != 0)
            .ok_or_else(|| anyhow::anyhow!("singular CM256 matrix"))?;
        matrix.swap(column, pivot);
        let inverse = gf_inverse(matrix[column][column]);
        for value in &mut matrix[column] {
            *value = gf_multiply(*value, inverse);
        }
        for row in 0..count {
            if row == column {
                continue;
            }
            let coefficient = matrix[row][column];
            if coefficient == 0 {
                continue;
            }
            let pivot_row = matrix[column].clone();
            for (target, source) in matrix[row].iter_mut().zip(pivot_row) {
                *target ^= gf_multiply(source, coefficient);
            }
        }
    }
    Ok(())
}

fn matrix_element(row: u8, first_repair_row: u8, column: u8) -> u8 {
    gf_divide(first_repair_row ^ column, row ^ column)
}

fn multiply_add(output: &mut [u8], input: &[u8], coefficient: u8) {
    if coefficient == 0 {
        return;
    }
    if coefficient == 1 {
        for (output, input) in output.iter_mut().zip(input) {
            *output ^= *input;
        }
        return;
    }
    let table = multiplication_table();
    let row = usize::from(coefficient) * 256;
    for (output, input) in output.iter_mut().zip(input) {
        *output ^= table[row + usize::from(*input)];
    }
}

fn gf_multiply(left: u8, right: u8) -> u8 {
    multiplication_table()[usize::from(right) * 256 + usize::from(left)]
}

fn gf_divide(numerator: u8, denominator: u8) -> u8 {
    if numerator == 0 {
        return 0;
    }
    assert_ne!(denominator, 0, "CM256 Cauchy denominator must be non-zero");
    let (logarithm, exponent) = gf_tables();
    let power = (u16::from(logarithm[usize::from(numerator)]) + 255
        - u16::from(logarithm[usize::from(denominator)]))
        % 255;
    exponent[usize::from(power)]
}

fn gf_inverse(value: u8) -> u8 {
    assert_ne!(value, 0, "zero has no GF(256) inverse");
    gf_divide(1, value)
}

fn multiplication_table() -> &'static [u8; 65_536] {
    static TABLE: OnceLock<Box<[u8; 65_536]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let (logarithm, exponent) = gf_tables();
        let mut table = Box::new([0_u8; 65_536]);
        for right in 1..256 {
            for left in 1..256 {
                let power = usize::from(logarithm[left]) + usize::from(logarithm[right]);
                table[right * 256 + left] = exponent[power % 255];
            }
        }
        table
    })
}

fn gf_tables() -> &'static ([u8; 256], [u8; 255]) {
    static TABLES: OnceLock<([u8; 256], [u8; 255])> = OnceLock::new();
    TABLES.get_or_init(|| {
        let mut logarithm = [0_u8; 256];
        let mut exponent = [0_u8; 255];
        let mut value = 1_u16;
        for power in 0..255_u16 {
            exponent[usize::from(power)] = value as u8;
            logarithm[usize::from(value as u8)] = power as u8;
            value <<= 1;
            if value & 0x100 != 0 {
                value ^= 0x14d;
            }
        }
        (logarithm, exponent)
    })
}

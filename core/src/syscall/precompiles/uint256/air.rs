use num::Zero;
use num::{BigUint, One};
use p3_air::{Air, AirBuilder, BaseAir};
use p3_field::AbstractField;
use p3_field::PrimeField32;
use p3_matrix::dense::RowMajorMatrix;
use p3_matrix::Matrix;
use serde::{Deserialize, Serialize};
use sp1_derive::AlignedBorrow;
use std::borrow::{Borrow, BorrowMut};
use std::mem::size_of;

use crate::air::{MachineAir, Polynomial, SP1AirBuilder};
use crate::bytes::event::ByteRecord;
use crate::memory::{MemoryCols, MemoryReadWriteCols};
use crate::memory::{MemoryReadCols, MemoryWriteCols};
use crate::operations::field::field_op::{FieldOpCols, FieldOperation};
use crate::operations::field::params::FieldParameters;
use crate::operations::field::params::{Limbs, NumLimbs};
use crate::runtime::{ExecutionRecord, Program, Syscall, SyscallCode};
use crate::runtime::{MemoryReadRecord, MemoryWriteRecord};
use crate::stark::MachineRecord;
use crate::syscall::precompiles::{uint256, SyscallContext};
use crate::utils::ec::uint256::U256Field;
use crate::utils::{
    bytes_to_words_le, limbs_from_access, limbs_from_prev_access, pad_rows, words_to_bytes_le,
    words_to_bytes_le_vec,
};

/// The number of columns in the Uint256MulCols.
const NUM_COLS: usize = size_of::<Uint256MulCols<u8>>();

/// The number of limbs it takes to represent a U256.
///
/// Note: this differs from what's in U256Field because we need 33 limbs to encode the modulus.
const NUM_PHYSICAL_LIMBS: usize = 32;

/// The number of words it takes to represent a U256.
///
/// Note: this differs from what's in U256Field because we need 33 limbs to encode the modulus.
const NUM_PHYSICAL_WORDS: usize = NUM_PHYSICAL_LIMBS / 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Uint256MulEvent {
    pub shard: u32,
    pub clk: u32,
    pub x_ptr: u32,
    pub x: Vec<u32>,
    pub y_ptr: u32,
    pub y: Vec<u32>,
    pub modulus: Vec<u32>,
    pub x_memory_records: [MemoryWriteRecord; NUM_PHYSICAL_WORDS],
    pub y_memory_records: [MemoryReadRecord; NUM_PHYSICAL_WORDS],
    pub modulus_memory_records: [MemoryReadRecord; NUM_PHYSICAL_WORDS],
}

#[derive(Default)]
pub struct Uint256MulChip;

impl Uint256MulChip {
    pub fn new() -> Self {
        Self
    }
}

/// A set of columns for the Uint256Mul operation.
#[derive(Debug, Clone, AlignedBorrow)]
#[repr(C)]
pub struct Uint256MulCols<T> {
    /// The shard number of the syscall.
    pub shard: T,

    /// The clock cycle of the syscall.
    pub clk: T,

    /// The pointer to the first input.
    pub x_ptr: T,

    /// The pointer to the second input, which is 16 words of (y, modulus).
    pub y_ptr: T,

    // Memory columns.
    // We read from x, we write the result to x as well.
    pub x_memory: [MemoryWriteCols<T>; NUM_PHYSICAL_WORDS],
    pub y_memory: [MemoryReadCols<T>; NUM_PHYSICAL_WORDS],
    pub modulus_memory: [MemoryReadCols<T>; NUM_PHYSICAL_WORDS],

    // Output values.
    pub output: FieldOpCols<T, U256Field>,

    pub is_real: T,
}

impl<F: PrimeField32> MachineAir<F> for Uint256MulChip {
    type Record = ExecutionRecord;
    type Program = Program;

    fn name(&self) -> String {
        "Uint256MulMod".to_string()
    }

    fn generate_trace(
        &self,
        input: &ExecutionRecord,
        output: &mut ExecutionRecord,
    ) -> RowMajorMatrix<F> {
        // Generate the trace rows & corresponding records for each chunk of events concurrently.
        let rows_and_records = input
            .uint256_mul_events
            .chunks(1)
            .map(|events| {
                let mut records = ExecutionRecord::default();
                let mut new_byte_lookup_events = Vec::new();

                let rows = events
                    .iter()
                    .map(|event| {
                        let mut row: [F; NUM_COLS] = [F::zero(); NUM_COLS];
                        let cols: &mut Uint256MulCols<F> = row.as_mut_slice().borrow_mut();

                        // Decode uint256 points
                        let x = BigUint::from_bytes_le(&words_to_bytes_le::<32>(&event.x));
                        let y = BigUint::from_bytes_le(&words_to_bytes_le::<32>(&event.y));
                        let modulus =
                            BigUint::from_bytes_le(&words_to_bytes_le::<32>(&event.modulus));
                        // Assign basic values to the columns.

                        cols.is_real = F::one();
                        cols.shard = F::from_canonical_u32(event.shard);
                        cols.clk = F::from_canonical_u32(event.clk);
                        cols.x_ptr = F::from_canonical_u32(event.x_ptr);
                        cols.y_ptr = F::from_canonical_u32(event.y_ptr);

                        // Memory columns.
                        // Populate memory columns.
                        for i in 0..NUM_PHYSICAL_WORDS {
                            // Populate the input_x columns.
                            cols.x_memory[i]
                                .populate(event.x_memory_records[i], &mut new_byte_lookup_events);
                            // Populate the input_y columns.
                            cols.y_memory[i]
                                .populate(event.y_memory_records[i], &mut new_byte_lookup_events);
                            cols.modulus_memory[i].populate(
                                event.modulus_memory_records[i],
                                &mut new_byte_lookup_events,
                            );
                        }

                        // Populate the output column.
                        cols.output.populate_with_modulus(
                            &mut new_byte_lookup_events,
                            event.shard,
                            &x,
                            &y,
                            &modulus,
                            FieldOperation::Mul,
                        );

                        row
                    })
                    .collect::<Vec<_>>();
                records.add_byte_lookup_events(new_byte_lookup_events);
                (rows, records)
            })
            .collect::<Vec<_>>();

        //  Generate the trace rows for each event.
        let mut rows = Vec::new();
        for (row, mut record) in rows_and_records {
            rows.extend(row);
            output.append(&mut record);
        }

        pad_rows(&mut rows, || {
            let mut row: [F; NUM_COLS] = [F::zero(); NUM_COLS];
            let cols: &mut Uint256MulCols<F> = row.as_mut_slice().borrow_mut();

            let x = BigUint::zero();
            let y = BigUint::zero();
            cols.output
                .populate(&mut vec![], 0, &x, &y, FieldOperation::Mul);

            row
        });

        // Convert the trace to a row major matrix.
        RowMajorMatrix::new(rows.into_iter().flatten().collect::<Vec<_>>(), NUM_COLS)
    }

    fn included(&self, shard: &Self::Record) -> bool {
        !shard.uint256_mul_events.is_empty()
    }
}

impl Syscall for Uint256MulChip {
    fn num_extra_cycles(&self) -> u32 {
        0
    }

    fn execute(&self, rt: &mut SyscallContext, arg1: u32, arg2: u32) -> Option<u32> {
        let x_ptr = arg1;
        if x_ptr % 4 != 0 {
            panic!();
        }
        let y_ptr = arg2;
        if y_ptr % 4 != 0 {
            panic!();
        }

        assert!(x_ptr != y_ptr);

        let x = rt.slice_unsafe(x_ptr, 8);

        let (y_memory_records_vec, y) = rt.mr_slice(y_ptr, 8);
        let y_memory_records = y_memory_records_vec.try_into().unwrap();

        let (modulus_memory_records_vec, modulus) = rt.mr_slice(y_ptr + 8 * 4, 8);
        let modulus_memory_records = modulus_memory_records_vec.try_into().unwrap();

        let uint256_x = BigUint::from_bytes_le(&words_to_bytes_le_vec(&x));
        let uint256_y = BigUint::from_bytes_le(&words_to_bytes_le_vec(&y));
        let uint256_modulus = BigUint::from_bytes_le(&words_to_bytes_le_vec(&modulus));

        // Perform the multiplication and take the result modulo the modulus.
        let result: BigUint = (uint256_x * uint256_y) % uint256_modulus;

        let mut result_bytes = result.to_bytes_le();
        result_bytes.resize(32, 0u8);

        // Convert the result to low endian u32 words.
        let result = bytes_to_words_le::<8>(&result_bytes);

        // write the state
        assert_eq!(result.len(), 8);
        let x_memory_records = rt.mw_slice(x_ptr, &result).try_into().unwrap();

        let shard = rt.current_shard();
        let clk = rt.clk;
        rt.record_mut().uint256_mul_events.push(Uint256MulEvent {
            shard,
            clk,
            x_ptr,
            x,
            y_ptr,
            y,
            modulus,
            x_memory_records,
            y_memory_records,
            modulus_memory_records,
        });

        None
    }
}

impl<F> BaseAir<F> for Uint256MulChip {
    fn width(&self) -> usize {
        NUM_COLS
    }
}

impl<AB> Air<AB> for Uint256MulChip
where
    AB: SP1AirBuilder,
    Limbs<AB::Var, <U256Field as NumLimbs>::Limbs>: Copy,
{
    fn eval(&self, builder: &mut AB) {
        let main = builder.main();
        let local = main.row_slice(0);
        let local: &Uint256MulCols<AB::Var> = (*local).borrow();

        let x_limbs = limbs_from_prev_access(&local.x_memory);
        let y_limbs = limbs_from_access(&local.y_memory);
        let modulus_limbs = limbs_from_access(&local.modulus_memory);

        // Evaluate the uint256 multiplication
        local.output.eval_with_modulus(
            builder,
            &x_limbs,
            &y_limbs,
            &modulus_limbs,
            local.shard,
            local.is_real,
        );

        // Assert that the output is equal to whats written to the memory record.
        for i in 0..NUM_PHYSICAL_LIMBS {
            builder
                .when(local.is_real)
                .assert_eq(local.output.result[i], local.x_memory[i / 4].value()[i % 4]);
        }

        // Read and write x.
        builder.eval_memory_access_slice(
            local.shard,
            local.clk.into(),
            local.x_ptr,
            &local.x_memory,
            local.is_real,
        );

        // Read y.
        builder.eval_memory_access_slice(
            local.shard,
            local.clk.into(),
            local.y_ptr,
            &[local.y_memory, local.modulus_memory].concat(),
            local.is_real,
        );

        // Receive the arguments.
        builder.receive_syscall(
            local.shard,
            local.clk,
            AB::F::from_canonical_u32(SyscallCode::UINT256_MUL.syscall_id()),
            local.x_ptr,
            local.y_ptr,
            local.is_real,
        );

        // Assert that is_real is a boolean.
        builder.assert_bool(local.is_real);
    }
}

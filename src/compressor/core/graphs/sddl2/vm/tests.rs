use super::*;
use alloc::string::ToString;
use alloc::vec;

/// Words of `family << 16 | opcode`.
fn code(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

const PUSH_ZERO: u32 = 0x0001_0001;
const PUSH_U32: u32 = 0x0001_0002;
const PUSH_I32: u32 = 0x0001_0003;
const PUSH_I64: u32 = 0x0001_0004;
const PUSH_TAG: u32 = 0x0001_0005;
const PUSH_POS: u32 = 0x0001_0080;
const PUSH_REMAINING: u32 = 0x0001_0081;
const PUSH_DEPTH: u32 = 0x0001_0082;
const TYPE_U8: u32 = 0x0001_0110;
const TYPE_U16LE: u32 = 0x0001_0112;
const TYPE_U32BE: u32 = 0x0001_0117;
const HALT: u32 = 0x0005_0001;
const EXPECT: u32 = 0x0005_0002;
const JUMP_IF: u32 = 0x0005_0003;
const SEGMENT_UNSPECIFIED: u32 = 0x000C_0001;
const SEGMENT_TAGGED: u32 = 0x000C_0002;
const FIXED_ARRAY: u32 = 0x0008_0001;
const STRUCTURE: u32 = 0x0008_0002;
const SIZEOF: u32 = 0x0008_0010;

fn run(words: &[u32], input: &[u8]) -> Result<Vec<Segment>, VmError> {
    execute(&code(words), input)
}

/// The value left on top of the stack by `words`, as a segment size of an
/// unspecified segment over `input`.
fn top_as_size(words: &[u32], input: &[u8]) -> Result<usize, VmError> {
    let mut program = words.to_vec();
    program.push(SEGMENT_UNSPECIFIED);
    run(&program, input).map(|segments| segments.first().map_or(0, |s| s.size_bytes))
}

#[test]
fn an_empty_program_makes_no_segment() {
    assert!(run(&[], b"abc").unwrap().is_empty());
    assert!(
        run(&[HALT, PUSH_ZERO, SEGMENT_UNSPECIFIED], b"abc")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn bytecode_must_be_whole_words() {
    assert_eq!(
        execute(&[1, 2, 3], &[]).unwrap_err(),
        VmError::InvalidBytecode
    );
}

#[test]
fn pushes_and_immediates_feed_segments() {
    let input = [0u8; 40];
    assert_eq!(top_as_size(&[PUSH_ZERO], &input), Ok(0));
    assert_eq!(top_as_size(&[PUSH_U32, 7], &input), Ok(7));
    assert_eq!(top_as_size(&[PUSH_I32, 9], &input), Ok(9));
    assert_eq!(
        top_as_size(
            &[PUSH_I32, (-3i32) as u32, PUSH_I32, 10, 0x0002_0001],
            &input
        ),
        Ok(7)
    );
    assert_eq!(top_as_size(&[PUSH_I64, 11, 0], &input), Ok(11));
    assert_eq!(top_as_size(&[PUSH_REMAINING], &input), Ok(40));
    assert_eq!(top_as_size(&[PUSH_POS], &input), Ok(0));
    assert_eq!(
        top_as_size(&[PUSH_ZERO, PUSH_ZERO, PUSH_DEPTH], &input),
        Ok(2)
    );
    assert_eq!(
        run(&[PUSH_U32], &[]).unwrap_err(),
        VmError::InvalidBytecode,
        "missing 32-bit immediate"
    );
    assert_eq!(
        run(&[PUSH_I64, 1], &[]).unwrap_err(),
        VmError::InvalidBytecode,
        "missing 64-bit immediate"
    );
}

#[test]
fn arithmetic_matches_the_reference_semantics() {
    let input = [0u8; 100];
    let binary = |op: u32, a: i32, b: i32| {
        top_as_size(&[PUSH_I32, a as u32, PUSH_I32, b as u32, op], &input)
    };
    let unary = |op: u32, a: i32| top_as_size(&[PUSH_I32, a as u32, op], &input);
    assert_eq!(binary(0x0002_0001, 40, 2), Ok(42));
    assert_eq!(binary(0x0002_0002, 40, 2), Ok(38));
    assert_eq!(binary(0x0002_0003, 40, 2), Ok(80));
    assert_eq!(binary(0x0002_0004, 40, 3), Ok(13));
    assert_eq!(binary(0x0002_0005, 40, 3), Ok(1));
    assert_eq!(unary(0x0002_0006, -5), Ok(5));
    assert_eq!(unary(0x0002_0007, -5), Ok(5));
    assert_eq!(binary(0x0002_0008, 12, 10), Ok(8));
    assert_eq!(binary(0x0002_0009, 12, 10), Ok(14));
    assert_eq!(binary(0x0002_000A, 12, 10), Ok(6));
    assert_eq!(unary(0x0002_000B, -4), Ok(3));
    assert_eq!(binary(0x0002_0004, 1, 0), Err(VmError::DivZero));
    assert_eq!(binary(0x0002_0005, 1, 0), Err(VmError::DivZero));
    let min = [PUSH_I64, 0, 0x8000_0000];
    for (op, expected) in [
        (0x0002_0006, VmError::MathOverflow),
        (0x0002_0007, VmError::MathOverflow),
    ] {
        let mut program = min.to_vec();
        program.push(op);
        assert_eq!(run(&program, &[]).unwrap_err(), expected);
    }
    let mut program = min.to_vec();
    program.extend([PUSH_I32, u32::MAX, 0x0002_0004]);
    assert_eq!(run(&program, &[]).unwrap_err(), VmError::MathOverflow);
    let max = [PUSH_I64, u32::MAX, 0x7FFF_FFFF];
    for op in [0x0002_0001, 0x0002_0003] {
        let mut program = max.to_vec();
        program.extend([PUSH_I32, 2, op]);
        assert_eq!(run(&program, &[]).unwrap_err(), VmError::MathOverflow);
    }
    let mut program = min.to_vec();
    program.extend([PUSH_I32, 1, 0x0002_0002]);
    assert_eq!(run(&program, &[]).unwrap_err(), VmError::MathOverflow);
    assert_eq!(
        run(&[0x0002_0099], &[]).unwrap_err(),
        VmError::InvalidBytecode
    );
}

#[test]
fn comparisons_and_logic_push_booleans() {
    let input = [0u8; 4];
    let binary = |op: u32, a: u32, b: u32| top_as_size(&[PUSH_U32, a, PUSH_U32, b, op], &input);
    for (op, results) in [
        (0x0003_0001, [0, 1, 0]),
        (0x0003_0002, [1, 0, 1]),
        (0x0003_0003, [1, 0, 0]),
        (0x0003_0004, [1, 1, 0]),
        (0x0003_0005, [0, 0, 1]),
        (0x0003_0006, [0, 1, 1]),
    ] {
        for ((a, b), expected) in [(1, 2), (2, 2), (3, 2)].into_iter().zip(results) {
            assert_eq!(binary(op, a, b), Ok(expected), "{op:#x} {a} {b}");
        }
    }
    assert_eq!(binary(0x0004_0001, 1, 2), Ok(1));
    assert_eq!(binary(0x0004_0001, 0, 2), Ok(0));
    assert_eq!(binary(0x0004_0002, 0, 0), Ok(0));
    assert_eq!(binary(0x0004_0002, 0, 2), Ok(1));
    assert_eq!(binary(0x0004_0003, 1, 2), Ok(0));
    assert_eq!(binary(0x0004_0003, 0, 2), Ok(1));
    assert_eq!(top_as_size(&[PUSH_ZERO, 0x0004_0004], &input), Ok(1));
    assert_eq!(top_as_size(&[PUSH_U32, 5, 0x0004_0004], &input), Ok(0));
    assert_eq!(
        run(&[0x0003_0099], &[]).unwrap_err(),
        VmError::InvalidBytecode
    );
    assert_eq!(
        run(&[0x0004_0099], &[]).unwrap_err(),
        VmError::InvalidBytecode
    );
}

#[test]
fn loads_read_every_width_and_endianness() {
    let input = [0x81u8, 0x02, 0x03, 0x84, 0x05, 0x06, 0x07, 0x88];
    let le = |n: usize| {
        let mut raw = [0u8; 8];
        raw[..n].copy_from_slice(&input[..n]);
        u64::from_le_bytes(raw)
    };
    let be = |n: usize| {
        let mut raw = [0u8; 8];
        raw[8 - n..].copy_from_slice(&input[..n]);
        u64::from_be_bytes(raw)
    };
    let cases: [(u32, i64); 12] = [
        (0x0001, le(1) as i64),
        (0x0002, le(1) as u8 as i8 as i64),
        (0x0010, le(2) as i64),
        (0x0011, le(2) as u16 as i16 as i64),
        (0x0020, le(4) as i64),
        (0x0021, le(4) as u32 as i32 as i64),
        (0x0030, le(8) as i64),
        (0x0110, be(2) as i64),
        (0x0111, be(2) as u16 as i16 as i64),
        (0x0120, be(4) as i64),
        (0x0121, be(4) as u32 as i32 as i64),
        (0x0130, be(8) as i64),
    ];
    for (opcode, expected) in cases {
        let program = [
            PUSH_ZERO,
            0x0006_0000 | opcode,
            PUSH_I64,
            expected as u32,
            (expected >> 32) as u32,
            0x0003_0001,
            EXPECT,
        ];
        assert!(
            run(&program, &input).is_ok(),
            "load {opcode:#x} = {expected}"
        );
    }
}

#[test]
fn loads_extend_signed_values_and_respect_bounds() {
    // The sign decides the comparison, whatever the width.
    let input = [0xFFu8; 8];
    for opcode in [0x0002u32, 0x0011, 0x0021, 0x0111, 0x0121, 0x0030, 0x0130] {
        let program = [
            PUSH_ZERO,
            0x0006_0000 | opcode,
            PUSH_ZERO,
            0x0003_0003,
            EXPECT,
        ];
        assert!(run(&program, &input).is_ok(), "{opcode:#x} is negative");
    }
    for opcode in [0x0001u32, 0x0010, 0x0020, 0x0110, 0x0120] {
        let program = [
            PUSH_ZERO,
            0x0006_0000 | opcode,
            PUSH_ZERO,
            0x0003_0005,
            EXPECT,
        ];
        assert!(run(&program, &input).is_ok(), "{opcode:#x} is positive");
    }
    assert_eq!(
        run(&[PUSH_I32, 5, 0x0006_0020], &input).unwrap_err(),
        VmError::LoadBounds
    );
    assert_eq!(
        run(&[PUSH_I32, u32::MAX, 0x0006_0001], &input).unwrap_err(),
        VmError::LoadBounds
    );
    assert_eq!(
        run(&[PUSH_I64, u32::MAX, 0x7FFF_FFFF, 0x0006_0001], &input).unwrap_err(),
        VmError::LoadBounds
    );
    assert_eq!(
        run(&[0x0006_0099], &input).unwrap_err(),
        VmError::InvalidBytecode
    );
}

#[test]
fn stack_operations_and_limits() {
    let input = [0u8; 10];
    // dup: 3 3 add = 6
    assert_eq!(
        top_as_size(&[PUSH_U32, 3, 0x0007_0001, 0x0002_0001], &input),
        Ok(6)
    );
    // swap: 2 5 swap sub = 3
    assert_eq!(
        top_as_size(
            &[PUSH_U32, 2, PUSH_U32, 5, 0x0007_0004, 0x0002_0002],
            &input
        ),
        Ok(3)
    );
    // drop
    assert_eq!(
        top_as_size(&[PUSH_U32, 4, PUSH_U32, 9, 0x0007_0003], &input),
        Ok(4)
    );
    // drop_if: condition 1 drops, condition 0 keeps
    assert_eq!(
        top_as_size(
            &[PUSH_U32, 4, PUSH_U32, 9, PUSH_U32, 1, 0x0007_0010],
            &input
        ),
        Ok(4)
    );
    assert_eq!(
        top_as_size(&[PUSH_U32, 4, PUSH_U32, 9, PUSH_ZERO, 0x0007_0010], &input),
        Ok(9)
    );
    assert_eq!(
        run(&[0x0007_0001], &[]).unwrap_err(),
        VmError::StackUnderflow
    );
    assert_eq!(
        run(&[PUSH_ZERO, 0x0007_0004], &[]).unwrap_err(),
        VmError::StackUnderflow
    );
    assert_eq!(
        run(&[0x0007_0003], &[]).unwrap_err(),
        VmError::StackUnderflow
    );
    assert_eq!(
        run(&[PUSH_U32, 1, 0x0007_0010], &[]).unwrap_err(),
        VmError::StackUnderflow
    );
    // A false condition drops nothing.
    assert!(run(&[PUSH_ZERO, 0x0007_0010], &[]).is_ok());
    // over and rot are not implemented.
    for opcode in [0x0007_0002, 0x0007_0005, 0x0007_0099] {
        assert_eq!(run(&[opcode], &[]).unwrap_err(), VmError::InvalidBytecode);
    }
    assert!(run(&vec![PUSH_ZERO; 256], &[]).is_ok());
    assert_eq!(
        run(&vec![PUSH_ZERO; 257], &[]).unwrap_err(),
        VmError::StackOverflow
    );
}

#[test]
fn control_flow_skips_instructions_with_their_immediates() {
    let input = [0u8; 20];
    // Skip `push.u32 5` (two words) when the condition holds: 7 stays.
    let skipped = [PUSH_U32, 7, PUSH_I32, 1, PUSH_I32, 1, JUMP_IF, PUSH_U32, 5];
    assert_eq!(top_as_size(&skipped, &input), Ok(7));
    let kept = [PUSH_U32, 7, PUSH_I32, 1, PUSH_ZERO, JUMP_IF, PUSH_U32, 5];
    assert_eq!(top_as_size(&kept, &input), Ok(5));
    // Skipping 64-bit and tag immediates.
    let wide = [
        PUSH_I32, 3, PUSH_I32, 1, JUMP_IF, PUSH_I64, 1, 2, PUSH_TAG, 4, PUSH_ZERO,
    ];
    assert!(run(&wide, &input).is_ok());
    // Skipping past the end, a negative count, a truncated immediate.
    assert_eq!(
        run(&[PUSH_I32, 5, PUSH_I32, 1, JUMP_IF], &input).unwrap_err(),
        VmError::InvalidBytecode
    );
    assert_eq!(
        run(&[PUSH_I32, u32::MAX, PUSH_I32, 1, JUMP_IF], &input).unwrap_err(),
        VmError::TypeMismatch
    );
    assert_eq!(
        run(&[PUSH_I32, 1, PUSH_I32, 1, JUMP_IF, PUSH_U32], &input).unwrap_err(),
        VmError::InvalidBytecode
    );
    assert_eq!(
        run(&[PUSH_I32, 1, PUSH_I32, 1, JUMP_IF, PUSH_I64, 1], &input).unwrap_err(),
        VmError::InvalidBytecode
    );
    assert_eq!(
        run(&[PUSH_ZERO, EXPECT], &input).unwrap_err(),
        VmError::ValidationFailed
    );
    assert!(run(&[PUSH_U32, 1, EXPECT, 0x0005_0004], &input).is_ok());
    assert_eq!(
        run(&[0x0005_0099], &input).unwrap_err(),
        VmError::InvalidBytecode
    );
}

#[test]
fn unknown_families_are_ignored_and_calls_rejected() {
    assert!(run(&[0x0099_0001, 0x0000_0000, 0xFFFF_FFFF], &[]).is_ok());
    assert_eq!(
        run(&[0x000B_0001], &[]).unwrap_err(),
        VmError::InvalidBytecode
    );
}

#[test]
fn variables_store_and_load_values_of_any_kind() {
    let input = [0u8; 9];
    let program = [
        PUSH_U32,
        9,
        PUSH_I32,
        2,
        0x0009_0001,
        PUSH_I32,
        2,
        0x0009_0002,
    ];
    assert_eq!(top_as_size(&program, &input), Ok(9));
    let typed = [
        PUSH_TAG,
        3,
        TYPE_U8,
        PUSH_I32,
        1,
        0x0009_0001,
        PUSH_I32,
        1,
        0x0009_0002,
        PUSH_I32,
        4,
        SEGMENT_TAGGED,
    ];
    assert_eq!(run(&typed, &input).unwrap().len(), 1);
    assert_eq!(
        run(&[PUSH_ZERO, 0x0009_0002], &[]).unwrap_err(),
        VmError::LoadBounds
    );
    assert_eq!(
        run(&[PUSH_I32, 256, 0x0009_0002], &[]).unwrap_err(),
        VmError::LoadBounds
    );
    assert_eq!(
        run(&[PUSH_I32, u32::MAX, 0x0009_0001], &[]).unwrap_err(),
        VmError::LoadBounds
    );
    assert_eq!(
        run(&[PUSH_I32, 255, 0x0009_0001], &[]).unwrap_err(),
        VmError::StackUnderflow
    );
    assert_eq!(
        run(&[0x0009_0099], &[]).unwrap_err(),
        VmError::InvalidBytecode
    );
}

#[test]
fn types_compose_into_arrays_and_structures() {
    let input = [0u8; 200];
    // [u16le; 3] has 6 bytes.
    assert_eq!(
        top_as_size(&[TYPE_U16LE, PUSH_I32, 3, FIXED_ARRAY, SIZEOF], &input),
        Ok(6)
    );
    // {u8, u32be, [u16le; 3]} has 11 bytes; ten of them as an array 110.
    let structure = [
        TYPE_U8,
        TYPE_U32BE,
        TYPE_U16LE,
        PUSH_I32,
        3,
        FIXED_ARRAY,
        PUSH_I32,
        3,
        STRUCTURE,
    ];
    let mut program = structure.to_vec();
    program.push(SIZEOF);
    assert_eq!(top_as_size(&program, &input), Ok(11));
    let mut program = structure.to_vec();
    program.extend([PUSH_I32, 10, FIXED_ARRAY, SIZEOF]);
    assert_eq!(top_as_size(&program, &input), Ok(110));
    // Nested structures add up.
    let mut program = structure.to_vec();
    program.extend([TYPE_U8, PUSH_I32, 2, STRUCTURE, SIZEOF]);
    assert_eq!(top_as_size(&program, &input), Ok(12));
    // The empty structure has no size.
    assert_eq!(top_as_size(&[PUSH_ZERO, STRUCTURE, SIZEOF], &input), Ok(0));
    // The count is truncated to 32 bits before it multiplies the width.
    assert_eq!(
        top_as_size(&[TYPE_U8, PUSH_I64, 0, 1, FIXED_ARRAY, SIZEOF], &input),
        Ok(0)
    );
    assert_eq!(
        run(
            &[
                TYPE_U8,
                PUSH_U32,
                70_000,
                FIXED_ARRAY,
                PUSH_U32,
                70_000,
                FIXED_ARRAY
            ],
            &[]
        )
        .unwrap_err(),
        VmError::MathOverflow
    );
    assert_eq!(
        run(&[TYPE_U8, PUSH_I32, u32::MAX, FIXED_ARRAY], &[]).unwrap_err(),
        VmError::TypeMismatch
    );
    assert_eq!(
        run(&[PUSH_ZERO, PUSH_I32, 1, FIXED_ARRAY], &[]).unwrap_err(),
        VmError::TypeMismatch
    );
    assert_eq!(
        run(&[TYPE_U8, PUSH_I32, 2, STRUCTURE], &[]).unwrap_err(),
        VmError::StackUnderflow
    );
    assert_eq!(
        run(&[PUSH_ZERO, PUSH_I32, 1, STRUCTURE], &[]).unwrap_err(),
        VmError::TypeMismatch
    );
    assert_eq!(
        run(&[PUSH_ZERO, SIZEOF], &[]).unwrap_err(),
        VmError::TypeMismatch
    );
    assert_eq!(
        run(&[0x0008_0099], &[]).unwrap_err(),
        VmError::InvalidBytecode
    );
    // A structure whose size overflows.
    let mut program = vec![0x0001_011A, PUSH_U32, u32::MAX, FIXED_ARRAY];
    for _ in 0..8 {
        program.extend(vec![0x0007_0001; 16]);
        program.extend([PUSH_I32, 17, STRUCTURE]);
    }
    assert_eq!(run(&program, &[]).unwrap_err(), VmError::MathOverflow);
}

#[test]
fn every_push_type_opcode_names_a_kind() {
    let kinds: Vec<(u16, usize)> = [
        (0x0100, 1),
        (0x0110, 1),
        (0x0111, 1),
        (0x0112, 2),
        (0x0113, 2),
        (0x0114, 2),
        (0x0115, 2),
        (0x0116, 4),
        (0x0117, 4),
        (0x0118, 4),
        (0x0119, 4),
        (0x011A, 8),
        (0x011B, 8),
        (0x011C, 8),
        (0x011D, 8),
        (0x0130, 1),
        (0x0131, 2),
        (0x0132, 2),
        (0x0133, 2),
        (0x0134, 2),
        (0x0135, 4),
        (0x0136, 4),
        (0x0137, 8),
        (0x0138, 8),
    ]
    .to_vec();
    for (opcode, size) in kinds {
        let kind = Kind::from_push_opcode(opcode).unwrap();
        assert_eq!(kind.size(), Some(size), "{opcode:#x}");
        let program = [0x0001_0000 | u32::from(opcode), SIZEOF];
        assert_eq!(top_as_size(&program, &[0u8; 8]), Ok(size));
    }
    assert_eq!(Kind::from_push_opcode(0x0101), None);
    assert_eq!(Kind::Structure.size(), None);
    assert_eq!(
        run(&[0x0001_0099], &[]).unwrap_err(),
        VmError::InvalidBytecode
    );
}

#[test]
fn segments_merge_register_tags_and_bound_the_input() {
    let input = [0u8; 40];
    let segments = run(
        &[
            PUSH_TAG,
            1,
            TYPE_U16LE,
            PUSH_I32,
            3,
            SEGMENT_TAGGED,
            PUSH_TAG,
            1,
            TYPE_U16LE,
            PUSH_I32,
            2,
            SEGMENT_TAGGED,
            PUSH_TAG,
            2,
            TYPE_U32BE,
            PUSH_I32,
            1,
            SEGMENT_TAGGED,
            PUSH_I32,
            4,
            SEGMENT_UNSPECIFIED,
            PUSH_I32,
            4,
            SEGMENT_UNSPECIFIED,
            PUSH_TAG,
            0,
            TYPE_U8,
            PUSH_I32,
            0,
            SEGMENT_TAGGED,
        ],
        &input,
    )
    .unwrap();
    let summary: Vec<_> = segments
        .iter()
        .map(|s| (s.tag, s.start_pos, s.size_bytes, s.ty.kind))
        .collect();
    assert_eq!(
        summary,
        vec![
            (1, 0, 10, Kind::U16Le),
            (2, 10, 4, Kind::U32Be),
            (0, 14, 8, Kind::Bytes),
        ]
    );
    // Same tag, different type; structures with equal members are distinct.
    let conflict = [
        PUSH_TAG,
        1,
        TYPE_U8,
        PUSH_I32,
        1,
        SEGMENT_TAGGED,
        PUSH_TAG,
        1,
        TYPE_U16LE,
        PUSH_I32,
        1,
        SEGMENT_TAGGED,
    ];
    assert_eq!(run(&conflict, &input).unwrap_err(), VmError::TypeMismatch);
    let two_structures = [
        PUSH_TAG,
        1,
        TYPE_U8,
        PUSH_I32,
        1,
        STRUCTURE,
        PUSH_I32,
        1,
        SEGMENT_TAGGED,
        PUSH_TAG,
        1,
        TYPE_U8,
        PUSH_I32,
        1,
        STRUCTURE,
        PUSH_I32,
        1,
        SEGMENT_TAGGED,
    ];
    assert_eq!(
        run(&two_structures, &input).unwrap_err(),
        VmError::TypeMismatch
    );
    assert_eq!(
        run(&[PUSH_I32, 41, SEGMENT_UNSPECIFIED], &input).unwrap_err(),
        VmError::SegmentBounds
    );
    assert_eq!(
        run(&[PUSH_I32, u32::MAX, SEGMENT_UNSPECIFIED], &input).unwrap_err(),
        VmError::TypeMismatch
    );
    assert_eq!(
        run(
            &[
                PUSH_TAG,
                1,
                TYPE_U32BE,
                PUSH_I64,
                u32::MAX,
                0x7FFF_FFFF,
                SEGMENT_TAGGED
            ],
            &input
        )
        .unwrap_err(),
        VmError::MathOverflow
    );
    assert_eq!(
        run(&[PUSH_ZERO, 0x000C_0099], &input).unwrap_err(),
        VmError::InvalidBytecode
    );
    assert_eq!(
        run(&[PUSH_ZERO, TYPE_U8, PUSH_ZERO, SEGMENT_TAGGED], &input).unwrap_err(),
        VmError::TypeMismatch
    );
    assert_eq!(
        run(&[PUSH_TAG, 1, PUSH_ZERO, PUSH_ZERO, SEGMENT_TAGGED], &input).unwrap_err(),
        VmError::TypeMismatch
    );
}

#[test]
fn segments_and_tags_are_capped() {
    // 524288 alternating one-byte segments fit; one more does not.
    let make = |count: usize| {
        let mut program = Vec::new();
        for n in 0..count {
            program.extend([
                PUSH_TAG,
                1 + (n % 2) as u32,
                TYPE_U8,
                PUSH_I32,
                1,
                SEGMENT_TAGGED,
            ]);
        }
        program
    };
    let full = make(SEGMENT_MAX);
    assert_eq!(
        run(&full, &vec![0u8; SEGMENT_MAX]).unwrap().len(),
        SEGMENT_MAX
    );
    let over = make(SEGMENT_MAX + 1);
    assert_eq!(
        run(&over, &vec![0u8; SEGMENT_MAX + 1]).unwrap_err(),
        VmError::LimitExceeded
    );
    // 32768 distinct tags fit; one more is refused.
    let tags = |count: u32| {
        let mut program = Vec::new();
        for tag in 1..=count {
            program.extend([PUSH_TAG, tag, TYPE_U8, PUSH_ZERO, SEGMENT_TAGGED]);
        }
        program
    };
    assert!(run(&tags(TAG_MAX as u32), &[]).is_ok());
    assert_eq!(
        run(&tags(TAG_MAX as u32 + 1), &[]).unwrap_err(),
        VmError::TypeMismatch
    );
}

#[test]
fn errors_and_types_describe_themselves() {
    for error in [
        VmError::StackOverflow,
        VmError::StackUnderflow,
        VmError::MathOverflow,
        VmError::TypeMismatch,
        VmError::LoadBounds,
        VmError::SegmentBounds,
        VmError::LimitExceeded,
        VmError::DivZero,
        VmError::InvalidBytecode,
        VmError::ValidationFailed,
    ] {
        assert!(!error.to_string().is_empty());
    }
    let ty = Type::primitive(Kind::U32Le);
    assert_eq!(ty.size(), Ok(4));
    let structure = Type {
        kind: Kind::Structure,
        width: 1,
        struct_data: None,
    };
    assert_eq!(structure.size(), Err(VmError::TypeMismatch));
    assert!(!ty.same_as(&structure));
}

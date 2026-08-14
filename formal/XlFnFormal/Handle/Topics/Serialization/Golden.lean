import XlFnFormal.Handle.Topics.Serialization.Safety

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Topics.Serialization

def zeroDigest : Vector UInt8 32 :=
  vectorOfBytes32 (List.replicate 32 (0 : UInt8)) (by simp)

def ffDigest : Vector UInt8 32 :=
  vectorOfBytes32 (List.replicate 32 (255 : UInt8)) (by simp)

def sequenceDigest : Vector UInt8 32 :=
  vectorOfBytes32 (List.range 32 |>.map UInt8.ofNat) (by simp)

def zeroEmptyKey : FormulaTopicKeyWire :=
  { sheetId := 0
    row := 0
    column := 0
    udfId := utf8Bytes ""
    argumentDigest := zeroDigest }

def i32BoundsKey : FormulaTopicKeyWire :=
  { sheetId := 4294967295
    row := -2147483648
    column := 2147483647
    udfId := utf8Bytes "TEST.HANDLE"
    argumentDigest := ffDigest }

def u64UnicodeKey : FormulaTopicKeyWire :=
  { sheetId := 18446744073709551615
    row := -1
    column := 0
    udfId := utf8Bytes "Unicode-日本語-😀"
    argumentDigest := sequenceDigest }

def embeddedSeparatorKey : FormulaTopicKeyWire :=
  { sheetId := 7
    row := 8
    column := 9
    udfId := utf8Bytes "A\u001fB\u001fC"
    argumentDigest := sequenceDigest }

theorem golden_zero_empty :
    formatRtdKey zeroEmptyKey =
      utf8Bytes "0\u001f0\u001f0\u001f\u001f0000000000000000000000000000000000000000000000000000000000000000" := by
  native_decide

theorem golden_i32_bounds :
    formatRtdKey i32BoundsKey =
      utf8Bytes "4294967295\u001f-2147483648\u001f2147483647\u001fTEST.HANDLE\u001fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff" := by
  native_decide

theorem golden_u64_unicode :
    formatRtdKey u64UnicodeKey =
      utf8Bytes "18446744073709551615\u001f-1\u001f0\u001fUnicode-日本語-😀\u001f000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f" := by
  native_decide

theorem golden_embedded_separator :
    formatRtdKey embeddedSeparatorKey =
      utf8Bytes "7\u001f8\u001f9\u001fA\u001fB\u001fC\u001f000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f" := by
  native_decide

theorem golden_vectors_parse_canonically :
    parseCanonicalRtdKey (formatRtdKey zeroEmptyKey) = some zeroEmptyKey ∧
    parseCanonicalRtdKey (formatRtdKey i32BoundsKey) = some i32BoundsKey ∧
    parseCanonicalRtdKey (formatRtdKey u64UnicodeKey) = some u64UnicodeKey ∧
    parseCanonicalRtdKey (formatRtdKey embeddedSeparatorKey) =
      some embeddedSeparatorKey := by
  exact ⟨parseCanonical_format _, parseCanonical_format _,
    parseCanonical_format _, parseCanonical_format _⟩

theorem golden_i32_bounds_are_valid_on_w32 :
    parseRtdKeyFor .w32 (formatRtdKey i32BoundsKey) = some i32BoundsKey := by
  exact parse_for_format_roundtrip .w32 i32BoundsKey (by native_decide)

theorem golden_u64_boundary_is_valid_on_w64 :
    parseRtdKeyFor .w64 (formatRtdKey u64UnicodeKey) = some u64UnicodeKey := by
  exact parse_for_format_roundtrip .w64 u64UnicodeKey (by native_decide)

theorem golden_u64_boundary_is_rejected_on_w32 :
    parseRtdKeyFor .w32 (formatRtdKey u64UnicodeKey) = none := by
  native_decide

theorem golden_wrong_suffix_separator_is_rejected :
    splitDigestSuffix (utf8Bytes "A" ++ [0x20] ++ formatDigest zeroDigest) = none := by
  native_decide

theorem golden_noncanonical_leading_zero_is_rejected :
    parseCanonicalRtdKey
        (utf8Bytes "00\u001f0\u001f0\u001f\u001f0000000000000000000000000000000000000000000000000000000000000000") =
      none := by
  native_decide

end XlFnFormal.Handle.Topics.Serialization

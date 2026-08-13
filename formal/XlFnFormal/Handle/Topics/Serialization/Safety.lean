import XlFnFormal.Handle.Topics.Serialization.Parser

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Topics.Serialization

theorem fromUTF8?_toByteArray (value : String) :
    String.fromUTF8? value.toByteArray = some value := by
  unfold String.fromUTF8?
  split
  · congr 1
  · rename_i h
    exact False.elim (h value.isValidUTF8)

theorem utf8Bytes_isUtf8 (value : String) :
    IsUtf8 (utf8Bytes value) := by
  unfold IsUtf8
  rw [utf8Bytes_toByteArray]
  rw [ByteArray.validateUTF8_eq_true_iff]
  exact value.isValidUTF8

theorem parseNatBytes_decimalNatBytes (value : Nat) :
    parseNatBytes (decimalNatBytes value) = some value := by
  unfold parseNatBytes
  rw [decimalNatBytes_toByteArray, fromUTF8?_toByteArray]
  exact Nat.toNat?_repr value

theorem parseIntBytes_decimalIntBytes (value : Int) :
    parseIntBytes (decimalIntBytes value) = some value := by
  unfold parseIntBytes
  rw [decimalIntBytes_toByteArray, fromUTF8?_toByteArray]
  exact Int.toInt?_repr value

theorem splitFirst_append_separator
    {field rest : ByteString}
    (hField : ∀ byte ∈ field, byte ≠ separator) :
    splitFirst (field ++ [separator] ++ rest) = some (field, rest) := by
  induction field with
  | nil => simp [splitFirst]
  | cons byte field ih =>
      have hByte : byte ≠ separator := hField byte (by simp)
      have hRest : ∀ b ∈ field, b ≠ separator := by
        intro b hb
        exact hField b (by simp [hb])
      simp only [List.cons_append, splitFirst, hByte, ↓reduceIte]
      rw [ih hRest]

theorem splitThree_structured
    {first second third rest : ByteString}
    (hFirst : ∀ byte ∈ first, byte ≠ separator)
    (hSecond : ∀ byte ∈ second, byte ≠ separator)
    (hThird : ∀ byte ∈ third, byte ≠ separator) :
    splitThree (first ++ [separator] ++ second ++ [separator] ++
      third ++ [separator] ++ rest) = some (first, second, third, rest) := by
  have h₁ := splitFirst_append_separator
    (field := first)
    (rest := second ++ [separator] ++ third ++ [separator] ++ rest)
    hFirst
  have h₂ := splitFirst_append_separator
    (field := second)
    (rest := third ++ [separator] ++ rest)
    hSecond
  have h₃ := splitFirst_append_separator
    (field := third)
    (rest := rest)
    hThird
  have h₂' : splitFirst (second ++ separator :: (third ++ separator :: rest)) =
      some (second, third ++ separator :: rest) := by
    simpa using h₂
  have h₃' : splitFirst (third ++ separator :: rest) = some (third, rest) := by
    simpa using h₃
  simp only [splitThree]
  rw [show first ++ [separator] ++ second ++ [separator] ++
      third ++ [separator] ++ rest =
      first ++ [separator] ++ (second ++ [separator] ++ third ++ [separator] ++ rest) by
        simp [List.append_assoc], h₁]
  simp [h₂', h₃']

theorem splitDigestSuffix_structured
    {udf digest : ByteString}
    (hDigest : digest.length = 64) :
    splitDigestSuffix (udf ++ [separator] ++ digest) = some (udf, digest) := by
  unfold splitDigestSuffix
  have hLength : 65 ≤ (udf ++ [separator] ++ digest).length := by
    simp [hDigest]
  simp only [List.length_append, List.length_cons]
  simp [hDigest]

theorem hexValue_hexNibble {value : Nat} (hValue : value < 16) :
    hexValue (hexNibble value) = some value := by
  unfold hexValue hexNibble
  by_cases hSmall : value < 10
  · have hModulo : 48 + value < 256 := by omega
    have hLower : 48 ≤ 48 + value := by omega
    have hUpper : 48 + value ≤ 57 := by omega
    simp [hSmall, hLower, hUpper, Nat.mod_eq_of_lt hModulo]
  · have hLarge : 10 ≤ value := by omega
    have hModulo : 87 + value < 256 := by omega
    have hLower : 97 ≤ 87 + value := by omega
    have hUpper : 87 + value ≤ 102 := by omega
    have hFirstFalse : ¬ (48 ≤ 87 + value ∧ 87 + value ≤ 57) := by omega
    simp [hSmall, hLower, hUpper, hFirstFalse,
      Nat.mod_eq_of_lt hModulo]

theorem parseHexByte_hexByte (value : UInt8) :
    parseHexByte (hexByte value) = some value := by
  simp only [parseHexByte, hexByte]
  have hByte := UInt8.toNat_lt value
  have hHigh : value.toNat / 16 < 16 := by omega
  have hLow : value.toNat % 16 < 16 := Nat.mod_lt _ (by decide)
  rw [hexValue_hexNibble hHigh, hexValue_hexNibble hLow]
  simp only [Option.some.injEq]
  apply UInt8.toNat.inj
  simp
  omega

theorem parseDigestPairs_format (bytes : ByteString) :
    parseDigestPairs bytes.length (bytes.flatMap hexByte) = some bytes := by
  induction bytes with
  | nil => simp [parseDigestPairs]
  | cons byte rest ih =>
      simp only [List.length_cons, List.flatMap_cons]
      change parseDigestPairs (Nat.succ rest.length)
        (hexByte byte ++ List.flatMap hexByte rest) = some (byte :: rest)
      rw [show hexByte byte =
          [hexNibble (byte.toNat / 16), hexNibble (byte.toNat % 16)] by rfl]
      have hParse :
          parseHexByte [hexNibble (byte.toNat / 16), hexNibble (byte.toNat % 16)] =
            some byte := by
        simpa [hexByte] using parseHexByte_hexByte byte
      simp only [parseDigestPairs, List.cons_append, List.nil_append]
      rw [hParse]
      simp [ih]

theorem parseDigest_format (digest : Vector UInt8 32) :
    parseDigest (formatDigest digest) = some digest := by
  cases digest with
  | mk array hSize =>
      have hLength : array.toList.length = 32 := by simpa using hSize
      have hPairs :
          parseDigestPairs 32 (array.toList.flatMap hexByte) = some array.toList := by
        rw [← hLength]
        exact parseDigestPairs_format array.toList
      unfold parseDigest
      change (match parseDigestPairs 32 (array.toList.flatMap hexByte) with
        | none => none
        | some parsed =>
            if h : parsed.length = 32 then some (vectorOfBytes32 parsed h) else none) =
        some ⟨array, hSize⟩
      rw [hPairs]
      simp only [hLength]
      congr 1

theorem formatDigest_length (digest : Vector UInt8 32) :
    (formatDigest digest).length = 64 := by
  simp only [formatDigest, List.length_flatMap, hexByte, List.length_cons,
    List.length_nil]
  have hLength : digest.toList.length = 32 := by simp
  have hSum : ∀ bytes : ByteString,
      (bytes.map (fun _ => 2)).sum = 2 * bytes.length := by
    intro bytes
    induction bytes with
    | nil => simp
    | cons byte rest ih =>
        simp only [List.map_cons, List.sum_cons, List.length_cons]
        rw [ih]
        omega
  rw [hSum digest.toList]
  omega

theorem parse_format_roundtrip
    (key : FormulaTopicKeyWire) :
    parseRtdKey (formatRtdKey key) = some key := by
  have hSheet := decimalNatBytes_no_separator key.sheetId
  have hRow := decimalIntBytes_no_separator key.row
  have hColumn := decimalIntBytes_no_separator key.column
  have hSheet' : ∀ byte ∈ decimalNatBytes key.sheetId, byte ≠ separator := by
    intro byte hByte hEqual
    apply hSheet
    simpa [hEqual] using hByte
  have hRow' : ∀ byte ∈ decimalIntBytes key.row, byte ≠ separator := by
    intro byte hByte hEqual
    apply hRow
    simpa [hEqual] using hByte
  have hColumn' : ∀ byte ∈ decimalIntBytes key.column, byte ≠ separator := by
    intro byte hByte hEqual
    apply hColumn
    simpa [hEqual] using hByte
  have hDigest := formatDigest_length key.argumentDigest
  have hSplit := splitThree_structured
    (first := decimalNatBytes key.sheetId)
    (second := decimalIntBytes key.row)
    (third := decimalIntBytes key.column)
    (rest := key.udfId ++ [separator] ++ formatDigest key.argumentDigest)
    hSheet' hRow' hColumn'
  unfold parseRtdKey formatRtdKey
  rw [show decimalNatBytes key.sheetId ++ [separator] ++
      decimalIntBytes key.row ++ [separator] ++ decimalIntBytes key.column ++
      [separator] ++ key.udfId ++ [separator] ++ formatDigest key.argumentDigest =
      decimalNatBytes key.sheetId ++ [separator] ++
        decimalIntBytes key.row ++ [separator] ++ decimalIntBytes key.column ++
      [separator] ++ (key.udfId ++ [separator] ++ formatDigest key.argumentDigest) by
        simp [List.append_assoc]]
  rw [hSplit]
  simp only
  rw [splitDigestSuffix_structured hDigest]
  simp only
  rw [parseNatBytes_decimalNatBytes, parseIntBytes_decimalIntBytes,
    parseIntBytes_decimalIntBytes, parseDigest_format]

theorem parseRtdKeyFor_sound
    {width : PointerWidth}
    {bytes : ByteString}
    {key : FormulaTopicKeyWire}
    (hParsed : parseRtdKeyFor width bytes = some key) :
    WellFormed width key := by
  cases hRaw : parseRtdKey bytes with
  | none =>
      simp [parseRtdKeyFor, hRaw] at hParsed
  | some parsed =>
      by_cases hWell : WellFormed width parsed
      · have hEqual : parsed = key := by
          simpa [parseRtdKeyFor, hRaw, hWell] using hParsed
        simpa [hEqual] using hWell
      · simp [parseRtdKeyFor, hRaw, hWell] at hParsed

theorem parse_for_format_roundtrip
    (width : PointerWidth)
    (key : FormulaTopicKeyWire)
    (hWellFormed : WellFormed width key) :
    parseRtdKeyFor width (formatRtdKey key) = some key := by
  unfold parseRtdKeyFor
  rw [parse_format_roundtrip key]
  simp [hWellFormed]

theorem format_injective
    {left right : FormulaTopicKeyWire}
    (hFormat : formatRtdKey left = formatRtdKey right) :
    left = right := by
  have hParsed := congrArg parseRtdKey hFormat
  rw [parse_format_roundtrip left, parse_format_roundtrip right] at hParsed
  exact Option.some.inj hParsed

end XlFnFormal.Handle.Topics.Serialization

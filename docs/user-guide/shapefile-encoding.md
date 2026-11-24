# Shapefile DBF language drivers

A valid `.cpg` takes precedence over the language driver ID (LDID, byte 29 of the `.dbf`
header). Without a usable `.cpg`, the following inventory describes RustGIS's LDID decoding.
All numbers in the marker column are hexadecimal. These are language-driver identifiers,
not the numeric code-page labels accepted in a `.cpg`.

## Writing

`share.exportLayer` and `share.exportTable` take an `encoding` parameter for a Shapefile
(`.shp` or `.zip`): an encoding name or code page number as a `.cpg` names it, such as
`GBK`, `936` or `UTF-8` (the default). The `.dbf`'s bytes, its header's language driver and
its `.cpg` file all say that encoding, and text columns are as wide as their longest value
in its bytes. An encoding a `.dbf` can't be written in is refused: the DOS pages dbase's
own adapter decodes (437, 850, 852, 861, 865), whose strict encoder would fail the whole
file on a character they reject, and `x-user-defined`. Pages 866 and 874 are written
through `encoding_rs`, like every other name. A character the encoding has no bytes for
is written as its numeric character reference (`&#128512;`) and counted in one notice
naming the encoding and the fields. Without the parameter, a Shapefile is written in
UTF-8 as it always was.

Export Features offers the same choice as **Text encoding** for a Shapefile. It starts at
GBK for a Simplified Chinese interface and Big5 for a Traditional Chinese one, and at
UTF-8 otherwise: ArcMap 10.x reads a UTF-8 `.dbf` as the system's code page and shows
Chinese text as mojibake, while QGIS and ArcGIS Pro read both.

## Decoded markers

| Code page | LDID markers |
| --- | --- |
| DOS CP437 | `01`, `09`, `0B`, `0D`, `0F`, `11`, `15`, `18`, `19`, `1B` |
| DOS CP850 | `02`, `0A`, `0E`, `10`, `12`, `14`, `16`, `1A`, `1D`, `25`, `37` |
| DOS CP852 | `1F`, `22`, `23`, `40`, `64`, `87` |
| DOS CP861 | `67` |
| DOS CP865 | `08`, `17`, `66` |
| DOS CP866 | `26`, `65` |
| Windows-874 (Thai) | `50`, `7C` |
| CP932 (Shift JIS) | `13`, `7B` |
| CP936 (GBK) | `4D`, `7A` |
| CP949 (Korean) | `4E`, `79` |
| CP950 (Big5) | `4F`, `78` |
| Windows-1250 | `C8` |
| Windows-1251 | `C9` |
| Windows-1252 | `03`, `57`, `58`, `59` |
| Windows-1253 | `CB` |
| Windows-1254 | `CA` |
| Windows-1255 | `7D` |
| Windows-1256 | `7E` |
| Windows-1257 | `CC` |
| Mac Roman (10000) | `04` |
| Mac Cyrillic (10007; see below) | `96` |
| UTF-8 (65001) | `F0` |
| No encoding metadata: UTF-8 default | `00` |

`57` deliberately means Windows-1252 in RustGIS. GDAL interprets it as ISO-8859-1;
changing to that interpretation would change bytes `80`–`9F`, such as the euro sign and
an en dash, into control characters. The regression test uses `80 96 9F` → `€–Ÿ`.

Mac Cyrillic is decoded with `encoding_rs`'s `x-mac-cyrillic` (Mac Ukrainian) decoder.
The shared Cyrillic letters are supported; this is not an exact mapping of every Mac
Cyrillic character. The test uses `8A E8 E5 E2` → `Киев`, which both mappings share.

## Recognized but not decoded

| Code page | LDID markers |
| --- | --- |
| DOS CP863 (French Canadian) | `1C`, `6C` |
| DOS CP860 (Portuguese) | `24` |
| DOS CP895 (Czech, Kamenický) | `68` |
| DOS CP620 (Polish, Mazovia) | `69` |
| DOS CP737 (Greek) | `6A`, `86` |
| DOS CP857 (Turkish) | `6B`, `88` |
| Mac Central European (10029) | `97` |

These markers emit a diagnostic naming the marker and code page, then use lossy UTF-8.
Unknown markers also emit a diagnostic and use lossy UTF-8. Valid UTF-8 survives, malformed
bytes become replacement characters, and the records remain available. An unusable `.cpg`
adds its own diagnostic before falling back to the LDID. Supplying a valid `.cpg` bypasses
the LDID and its unsupported-code-page diagnostic.

The existing transitive `yore` 1.4.0 package has CP737, CP857, CP860 and CP863 decoders,
but RustGIS's current `dbase` adapter does not expose them. Making these available would
require decoder integration and a coordinated direct dependency declaration; their presence
in the lockfile does not make the corresponding LDIDs supported today. No local conversion
tables or system programs are needed for the supported decoders above.

## References and regression coverage

- Marker inventory: GDAL 3.11.4's
  [`GetEncodingFromLDIDNumber`](https://github.com/OSGeo/gdal/blob/v3.11.4/ogr/ogrsf_frmts/shape/ogrshapelayer.cpp#L300),
  plus `09`, `7D`, `7E` and `F0` from
  [`dbase` 0.8.0's `CodePageMark`](https://docs.rs/dbase/0.8.0/dbase/enum.CodePageMark.html).
- [GDAL's Shapefile encoding documentation](https://gdal.org/en/stable/drivers/vector/shapefile.html#encoding)
  describes `.cpg` precedence and its different `57` interpretation.
- Character mapping references: Unicode's
  [Microsoft code-page mappings](https://www.unicode.org/Public/MAPPINGS/VENDORS/MICSFT/)
  and [Apple mappings](https://www.unicode.org/Public/MAPPINGS/VENDORS/APPLE/).
- [`yore`'s `CodePage` implementations](https://docs.rs/yore/1.4.0/yore/trait.CodePage.html#implementors)
  list the available legacy decoders and lossy decoding API.

`crates/data/tests/shapefile_encoding.rs` specifies fixed bytes and Unicode expectations
independently of RustGIS's mapping. Its matrix covers every marker above, names and values,
ZIP and loose-file imports, a genuinely sampled streamed read, a complete read, and ordered
chunk traversal. It also checks `.cpg` precedence, fallback diagnostics, and malformed
multibyte text without discarding records. Test files are generated locally; the tests do
not download data or call external converters.

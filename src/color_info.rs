//! The `ndi_color_info` element of video frame metadata.

use crate::{Error, Result};

/// The color description a video frame carries in its `ndi_color_info`
/// metadata.
///
/// NDI 6 describes a frame's colorimetry with one element in its metadata,
/// standing alone or inside an `ndi_metadata_group`:
///
/// ```xml
/// <ndi_color_info primaries="bt_2020" transfer="bt_2100_pq" matrix="bt_2020"/>
/// ```
///
/// Each attribute may be missing, and its accessor then returns `None`. A
/// missing `matrix` means BT.709 and a missing `transfer` means SDR, which is
/// how image encoding reads them; missing `primaries` are unknown.
///
/// Every video frame's `color_info` method reads it, for example
/// [`VideoFrame::color_info`](crate::VideoFrame::color_info). Measured with the
/// 6.4 SDK, the SDK passes the element through unchanged to receivers in
/// every color format, YUV and RGB, but does not apply it: it converts
/// between YUV and RGB with BT.709 coefficients whatever the element says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColorInfo {
    primaries: Option<ColorPrimaries>,
    transfer: Option<TransferFunction>,
    matrix: Option<YuvMatrix>,
}

impl ColorInfo {
    /// The color primaries, or `None` when the element does not give them.
    #[must_use]
    pub const fn primaries(&self) -> Option<ColorPrimaries> {
        self.primaries
    }

    /// The transfer function, or `None` when the element does not give one,
    /// which means SDR.
    #[must_use]
    pub const fn transfer(&self) -> Option<TransferFunction> {
        self.transfer
    }

    /// The YUV matrix, or `None` when the element does not give one, which
    /// means BT.709.
    #[must_use]
    pub const fn matrix(&self) -> Option<YuvMatrix> {
        self.matrix
    }

    /// Whether the transfer function is HDR: [`TransferFunction::Pq`] or
    /// [`TransferFunction::Hlg`].
    #[must_use]
    pub const fn is_hdr(&self) -> bool {
        matches!(
            self.transfer,
            Some(TransferFunction::Pq | TransferFunction::Hlg)
        )
    }
}

/// The color primaries of a [`ColorInfo`].
///
/// This enum is marked `#[non_exhaustive]` so that primaries a later SDK
/// names can be added without a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ColorPrimaries {
    /// ITU-R BT.601: `bt_601`.
    Bt601,
    /// ITU-R BT.709: `bt_709`.
    Bt709,
    /// ITU-R BT.2020: `bt_2020`, or `bt_2100`, whose primaries are the same.
    Bt2020,
}

/// The transfer function of a [`ColorInfo`].
///
/// This enum is marked `#[non_exhaustive]` so that transfer functions a later
/// SDK names can be added without a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TransferFunction {
    /// The SDR transfer of ITU-R BT.601: `bt_601`.
    Bt601,
    /// The SDR transfer of ITU-R BT.709: `bt_709`.
    Bt709,
    /// The SDR transfer of ITU-R BT.2020: `bt_2020`.
    Bt2020,
    /// Hybrid log-gamma, ITU-R BT.2100: `bt_2100_hlg`.
    Hlg,
    /// Perceptual quantization, ITU-R BT.2100 and SMPTE ST 2084:
    /// `bt_2100_pq`.
    Pq,
}

/// The matrix that relates YCbCr to RGB, in a [`ColorInfo`] and in image
/// encoding's color policy.
///
/// This enum is marked `#[non_exhaustive]` so that matrices a later SDK names
/// can be added without a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum YuvMatrix {
    /// ITU-R BT.601: K<sub>R</sub> 0.299, K<sub>B</sub> 0.114. `bt_601`.
    Bt601,
    /// ITU-R BT.709: K<sub>R</sub> 0.2126, K<sub>B</sub> 0.0722. `bt_709`.
    Bt709,
    /// ITU-R BT.2020 non-constant luminance: K<sub>R</sub> 0.2627,
    /// K<sub>B</sub> 0.0593. `bt_2020`, or `bt_2100`, which uses the same
    /// matrix.
    Bt2020,
}

/// The element's name, which every scanned start tag is compared with.
const ELEMENT: &[u8] = b"ndi_color_info";

/// The most of an unreadable value an error message quotes.
const QUOTED_LIMIT: usize = 40;

/// Read the `ndi_color_info` element of a frame's metadata.
///
/// Returns `Ok(None)` when there is no metadata or no element, and the color
/// description when every element in the text gives the same one. The
/// element may stand anywhere in the text, alone or inside an
/// `ndi_metadata_group`; comments, CDATA sections and processing instructions
/// are skipped, and an unterminated one runs to the end of the text, as in XML.
///
/// # Errors
///
/// [`Error::InvalidFrame`] naming the attribute and its value when an
/// element is malformed or truncated, gives an attribute twice, gives a value
/// outside the SDK's list, or differs from an earlier element.
pub(crate) fn parse(metadata: Option<&str>) -> Result<Option<ColorInfo>> {
    let Some(text) = metadata else {
        return Ok(None);
    };
    let bytes = text.as_bytes();
    let mut found = None;
    let mut at = 0;
    while let Some(offset) = bytes[at..].iter().position(|&byte| byte == b'<') {
        let tag = &bytes[at + offset..];
        at += offset
            + if let Some(body) = tag.strip_prefix(b"<!--") {
                match find(body, b"-->") {
                    Some(end) => 4 + end + 3,
                    None => break,
                }
            } else if let Some(body) = tag.strip_prefix(b"<![CDATA[") {
                match find(body, b"]]>") {
                    Some(end) => 9 + end + 3,
                    None => break,
                }
            } else if let Some(body) = tag.strip_prefix(b"<?") {
                match find(body, b"?>") {
                    Some(end) => 2 + end + 2,
                    None => break,
                }
            } else if let Some(attributes) = start_tag(tag) {
                let (info, len) = element(attributes)?;
                if let Some(earlier) = found.filter(|&earlier| earlier != info) {
                    return Err(Error::InvalidFrame(format!(
                        "The frame's metadata holds ndi_color_info elements that differ: {earlier:?} and {info:?}"
                    )));
                }
                found = Some(info);
                1 + ELEMENT.len() + len
            } else {
                1
            };
    }
    Ok(found)
}

/// The text after `<ndi_color_info` when `tag` starts that element's start
/// tag: the name must end there, at whitespace, `/`, `>` or the end of a
/// truncated text.
fn start_tag(tag: &[u8]) -> Option<&[u8]> {
    let after = tag.strip_prefix(b"<")?.strip_prefix(ELEMENT)?;
    match after.first() {
        Some(byte) if !byte.is_ascii_whitespace() && !matches!(byte, b'/' | b'>') => None,
        _ => Some(after),
    }
}

/// Read one element's attributes, from just after its name, returning the
/// description and the length read through the tag's closing `>`.
fn element(text: &[u8]) -> Result<(ColorInfo, usize)> {
    let mut info = ColorInfo {
        primaries: None,
        transfer: None,
        matrix: None,
    };
    let mut at = 0;
    loop {
        at += whitespace(&text[at..]);
        let rest = &text[at..];
        if rest.starts_with(b"/>") {
            return Ok((info, at + 2));
        }
        if rest.starts_with(b">") {
            return Ok((info, at + 1));
        }
        if rest.is_empty() {
            return Err(malformed("is not closed"));
        }
        let name_len = rest
            .iter()
            .position(|&byte| byte.is_ascii_whitespace() || matches!(byte, b'=' | b'/' | b'>'))
            .unwrap_or(rest.len());
        if name_len == 0 {
            return Err(malformed(format!(
                "has {} where an attribute should begin",
                quoted(&rest[..1])
            )));
        }
        let name = &rest[..name_len];
        at += name_len;
        at += whitespace(&text[at..]);
        if text.get(at) != Some(&b'=') {
            return Err(malformed(format!(
                "gives attribute {} without a value",
                quoted(name)
            )));
        }
        at += 1;
        at += whitespace(&text[at..]);
        let quote = match text.get(at) {
            Some(&quote @ (b'"' | b'\'')) => quote,
            _ => {
                return Err(malformed(format!(
                    "gives attribute {} an unquoted value",
                    quoted(name)
                )))
            }
        };
        at += 1;
        let Some(value_len) = text[at..].iter().position(|&byte| byte == quote) else {
            return Err(malformed(format!(
                "does not close the value of attribute {}",
                quoted(name)
            )));
        };
        let value = &text[at..at + value_len];
        at += value_len + 1;
        match name {
            b"primaries" => set(&mut info.primaries, name, value, |value| match value {
                b"bt_601" => Some(ColorPrimaries::Bt601),
                b"bt_709" => Some(ColorPrimaries::Bt709),
                b"bt_2020" | b"bt_2100" => Some(ColorPrimaries::Bt2020),
                _ => None,
            })?,
            b"transfer" => set(&mut info.transfer, name, value, |value| match value {
                b"bt_601" => Some(TransferFunction::Bt601),
                b"bt_709" => Some(TransferFunction::Bt709),
                b"bt_2020" => Some(TransferFunction::Bt2020),
                b"bt_2100_hlg" => Some(TransferFunction::Hlg),
                b"bt_2100_pq" => Some(TransferFunction::Pq),
                _ => None,
            })?,
            b"matrix" => set(&mut info.matrix, name, value, |value| match value {
                b"bt_601" => Some(YuvMatrix::Bt601),
                b"bt_709" => Some(YuvMatrix::Bt709),
                b"bt_2020" | b"bt_2100" => Some(YuvMatrix::Bt2020),
                _ => None,
            })?,
            // Attributes the SDK does not define are no concern of color.
            _ => {}
        }
    }
}

/// Store an attribute's value, refusing a second one and a value outside the
/// SDK's list.
fn set<T>(
    slot: &mut Option<T>,
    name: &[u8],
    value: &[u8],
    read: impl FnOnce(&[u8]) -> Option<T>,
) -> Result<()> {
    if slot.is_some() {
        return Err(malformed(format!("gives attribute {} twice", quoted(name))));
    }
    *slot = Some(read(value).ok_or_else(|| {
        Error::InvalidFrame(format!(
            "The frame's ndi_color_info gives attribute {} the value {}, which is not one the NDI SDK defines",
            quoted(name),
            quoted(value)
        ))
    })?);
    Ok(())
}

fn malformed(problem: impl std::fmt::Display) -> Error {
    Error::InvalidFrame(format!(
        "The frame's ndi_color_info metadata cannot be read: the element {problem}"
    ))
}

/// The length of the XML whitespace at the start of `text`.
fn whitespace(text: &[u8]) -> usize {
    text.iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(text.len())
}

/// The offset of the first `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Metadata text for a message, quoted and cut short.
fn quoted(text: &[u8]) -> String {
    if text.len() > QUOTED_LIMIT {
        format!(
            "\"{}…\" ({} bytes)",
            String::from_utf8_lossy(&text[..QUOTED_LIMIT]),
            text.len()
        )
    } else {
        format!("\"{}\"", String::from_utf8_lossy(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Result<Option<ColorInfo>> {
        parse(Some(text))
    }

    fn info(
        primaries: Option<ColorPrimaries>,
        transfer: Option<TransferFunction>,
        matrix: Option<YuvMatrix>,
    ) -> ColorInfo {
        ColorInfo {
            primaries,
            transfer,
            matrix,
        }
    }

    fn unreadable(text: &str) -> String {
        match read(text) {
            Err(Error::InvalidFrame(message)) => message,
            other => panic!("{text:?} must be refused, got {other:?}"),
        }
    }

    const HLG_2100: ColorInfo = ColorInfo {
        primaries: Some(ColorPrimaries::Bt2020),
        transfer: Some(TransferFunction::Hlg),
        matrix: Some(YuvMatrix::Bt2020),
    };

    #[test]
    fn no_metadata_or_no_element_is_none() {
        assert_eq!(parse(None).unwrap(), None);
        for text in [
            "",
            "<ndi_metadata_group/>",
            "<ndi_color_infos primaries=\"x\"/>",
            "<my_ndi_color_info primaries=\"bt_709\"/>",
            "ndi_color_info primaries=\"bt_709\"",
            "<!-- <ndi_color_info primaries=\"bt_9\"/> -->",
            "<![CDATA[<ndi_color_info primaries=\"bt_9\"/>]]>",
            "<!-- unterminated <ndi_color_info primaries=\"bt_9\"/>",
            "<![CDATA[ unterminated <ndi_color_info primaries=\"bt_9\"/>",
            "</ndi_color_info>",
        ] {
            assert_eq!(read(text).unwrap(), None, "{text:?}");
        }
    }

    #[test]
    fn processing_instruction_text_does_not_describe_the_frame() {
        for text in [
            "<?camera <ndi_color_info transfer='bt_2100_pq'/> ?>",
            "<?camera <ndi_color_info matrix='unknown'/> ?>",
            "<?camera <ndi_color_info transfer='bt_2100_pq'/>",
        ] {
            assert_eq!(read(text).unwrap(), None, "{text:?}");
        }
        let sdr = "<ndi_color_info transfer='bt_709'/>";
        let instruction = "<?camera <ndi_color_info transfer='bt_2100_pq'/> ?>";
        for text in [format!("{instruction}{sdr}"), format!("{sdr}{instruction}")] {
            assert_eq!(read(&text).unwrap(), read(sdr).unwrap(), "{text:?}");
        }
        // XML ends an instruction at its first ?>, even inside quotes.
        let text = format!("<?camera value='?>{sdr}");
        assert_eq!(read(&text).unwrap(), read(sdr).unwrap());
    }

    #[test]
    fn the_sdk_example_reads_in_any_attribute_order_and_quote_style() {
        // As the 6.4 SDK's HDR sender example writes it.
        let sdk = "<ndi_color_info \t transfer=\"bt_2100_hlg\" \t matrix=\"bt_2100\" \t primaries=\"bt_2100\" /> ";
        assert_eq!(read(sdk).unwrap(), Some(HLG_2100));
        for text in [
            "<ndi_color_info primaries='bt_2020' transfer='bt_2100_hlg' matrix='bt_2020'/>",
            "<ndi_color_info matrix=\"bt_2020\" primaries='bt_2100' transfer=\"bt_2100_hlg\">",
            "<ndi_color_info\nmatrix = \"bt_2020\"\r\n primaries\t=\t'bt_2020'transfer=\"bt_2100_hlg\"></ndi_color_info>",
            "<ndi_metadata_group><ndi_color_info primaries=\"bt_2020\" transfer=\"bt_2100_hlg\" matrix=\"bt_2020\"/></ndi_metadata_group>",
            "<!-- <ndi_color_info primaries=\"bt_601\"/> --><ndi_color_info primaries=\"bt_2020\" transfer=\"bt_2100_hlg\" matrix=\"bt_2020\" extra=\"ignored\"/>",
            "<![CDATA[ <ndi_color_info/> ]]><ndi_color_info primaries=\"bt_2020\" transfer=\"bt_2100_hlg\" matrix=\"bt_2020\"/>",
        ] {
            assert_eq!(read(text).unwrap(), Some(HLG_2100), "{text:?}");
        }
    }

    #[test]
    fn every_sdk_value_reads_and_missing_attributes_are_none() {
        for (value, primaries, matrix) in [
            ("bt_601", ColorPrimaries::Bt601, YuvMatrix::Bt601),
            ("bt_709", ColorPrimaries::Bt709, YuvMatrix::Bt709),
            ("bt_2020", ColorPrimaries::Bt2020, YuvMatrix::Bt2020),
            ("bt_2100", ColorPrimaries::Bt2020, YuvMatrix::Bt2020),
        ] {
            assert_eq!(
                read(&format!("<ndi_color_info primaries=\"{value}\"/>")).unwrap(),
                Some(info(Some(primaries), None, None))
            );
            assert_eq!(
                read(&format!("<ndi_color_info matrix=\"{value}\"/>")).unwrap(),
                Some(info(None, None, Some(matrix)))
            );
        }
        for (value, transfer) in [
            ("bt_601", TransferFunction::Bt601),
            ("bt_709", TransferFunction::Bt709),
            ("bt_2020", TransferFunction::Bt2020),
            ("bt_2100_hlg", TransferFunction::Hlg),
            ("bt_2100_pq", TransferFunction::Pq),
        ] {
            let read = read(&format!("<ndi_color_info transfer=\"{value}\"/>"))
                .unwrap()
                .unwrap();
            assert_eq!(read, info(None, Some(transfer), None));
            assert_eq!(
                read.is_hdr(),
                matches!(transfer, TransferFunction::Hlg | TransferFunction::Pq)
            );
        }
        let empty = read("<ndi_color_info/>").unwrap().unwrap();
        assert_eq!(empty, info(None, None, None));
        assert!(!empty.is_hdr());
    }

    #[test]
    fn repeats_must_agree() {
        let same = "<ndi_color_info transfer=\"bt_2100_pq\"/><ndi_metadata_group><ndi_color_info transfer='bt_2100_pq'/></ndi_metadata_group>";
        assert_eq!(
            read(same).unwrap(),
            Some(info(None, Some(TransferFunction::Pq), None))
        );
        for text in [
            "<ndi_color_info transfer=\"bt_2100_pq\"/><ndi_color_info transfer=\"bt_2100_hlg\"/>",
            "<ndi_color_info transfer=\"bt_2100_pq\"/><ndi_color_info transfer=\"bt_2100_pq\" primaries=\"bt_2020\"/>",
        ] {
            assert!(unreadable(text).contains("differ"), "{text:?}");
        }
    }

    #[test]
    fn unreadable_elements_are_refused_naming_the_attribute_and_value() {
        for (text, needles) in [
            (
                "<ndi_color_info matrix=\"bt_2021\"/>",
                &["\"matrix\"", "\"bt_2021\""][..],
            ),
            (
                "<ndi_color_info transfer=\"BT_709\"/>",
                &["\"transfer\"", "\"BT_709\""],
            ),
            (
                "<ndi_color_info primaries=\"\"/>",
                &["\"primaries\"", "\"\""],
            ),
            (
                "<ndi_color_info transfer=\"bt_2100\"/>",
                &["\"transfer\"", "\"bt_2100\""],
            ),
            (
                "<ndi_color_info matrix=\"bt_709\" matrix=\"bt_709\"/>",
                &["\"matrix\"", "twice"],
            ),
            (
                "<ndi_color_info matrix=bt_709/>",
                &["\"matrix\"", "unquoted"],
            ),
            (
                "<ndi_color_info matrix/>",
                &["\"matrix\"", "without a value"],
            ),
            (
                "<ndi_color_info matrix=\"bt_709'/>",
                &["\"matrix\"", "does not close"],
            ),
            ("<ndi_color_info matrix=\"bt_709\"", &["not closed"]),
            (
                "<ndi_color_info primaries=\"bt_20",
                &["\"primaries\"", "does not close"],
            ),
            ("<ndi_color_info ", &["not closed"]),
            ("<ndi_color_info\t", &["not closed"]),
            ("<ndi_color_info", &["not closed"]),
            (
                "<ndi_color_info = \"bt_709\"/>",
                &["\"=\"", "attribute should begin"],
            ),
            ("<ndi_color_info / >", &["\"/\"", "attribute should begin"]),
        ] {
            let message = unreadable(text);
            for needle in needles {
                assert!(message.contains(needle), "{text:?}: {message}");
            }
        }
        let long = format!("<ndi_color_info matrix=\"{}\"/>", "x".repeat(1000));
        let message = unreadable(&long);
        assert!(
            message.contains("(1000 bytes)") && message.len() < 200,
            "{message}"
        );
    }

    #[test]
    fn metadata_at_the_length_limit_reads_in_linear_time() {
        let limit = crate::frames::MAX_METADATA_BYTES - 1;
        let element = "<ndi_color_info transfer=\"bt_2100_pq\"/>";
        for filler in ["<", "<!", "<!-", "<n", "<![CDATA", " "] {
            let mut text = filler.repeat((limit - element.len()) / filler.len());
            text.push_str(element);
            let start = std::time::Instant::now();
            assert_eq!(
                read(&text).unwrap(),
                Some(info(None, Some(TransferFunction::Pq), None)),
                "{filler:?}"
            );
            assert!(
                start.elapsed() < std::time::Duration::from_secs(5),
                "{filler:?}"
            );
        }
        for prefix in ["<!--", "<![CDATA[", "<?camera "] {
            let unterminated = format!("{prefix}{}", "<ndi_color_info/>".repeat(limit / 20));
            assert_eq!(read(&unterminated).unwrap(), None);
        }
    }

    #[test]
    fn mutated_metadata_never_panics() {
        let valid = [
            "<ndi_color_info transfer=\"bt_2100_hlg\" matrix=\"bt_2100\" primaries=\"bt_2100\"/>",
            "<ndi_metadata_group><!-- c --><ndi_color_info primaries='bt_709'></ndi_color_info><![CDATA[x]]></ndi_metadata_group>",
            "<ndi_color_info matrix=\"bt_601\" />é<ndi_color_info matrix=\"bt_601\"/>",
            "<?camera <ndi_color_info matrix='unknown'/> ?>é<ndi_color_info matrix='bt_601'/>",
        ];
        let alphabet = "<>?/=\"' \t!-[]CDATAndi_color_infobt_2100_pqé";
        let alphabet: Vec<char> = alphabet.chars().collect();
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for round in 0..20_000 {
            let mut text: Vec<char> = valid[round % valid.len()].chars().collect();
            for _ in 0..1 + next() % 4 {
                let at = (next() as usize) % (text.len() + 1);
                match next() % 3 {
                    0 if at < text.len() => {
                        text.remove(at);
                    }
                    1 => text.insert(at, alphabet[(next() as usize) % alphabet.len()]),
                    _ => text.truncate(at),
                }
            }
            let text: String = text.into_iter().collect();
            let _ = read(&text);
        }
    }
}

use std::io::Cursor;

use image::{ImageEncoder, codecs::jpeg::JpegEncoder, codecs::webp::WebPEncoder};
use magi_browser_authority::{BrowserNormalizedRect, BrowserScreenshotFormat};

pub(crate) fn crop_browser_screenshot(
    bytes: &[u8],
    format: BrowserScreenshotFormat,
    clip: BrowserNormalizedRect,
    quality: Option<u8>,
) -> Result<Vec<u8>, String> {
    let image_format = image_format(format);
    let image = image::load_from_memory_with_format(bytes, image_format)
        .map_err(|error| format!("解析浏览器截图失败: {error}"))?;
    let image_width = image.width();
    let image_height = image.height();
    let left = (clip.x * f64::from(image_width))
        .floor()
        .clamp(0.0, f64::from(image_width.saturating_sub(1))) as u32;
    let top = (clip.y * f64::from(image_height))
        .floor()
        .clamp(0.0, f64::from(image_height.saturating_sub(1))) as u32;
    let crop_width =
        ((clip.width * f64::from(image_width)).round().max(1.0) as u32).min(image_width - left);
    let crop_height =
        ((clip.height * f64::from(image_height)).round().max(1.0) as u32).min(image_height - top);
    if left >= image_width || top >= image_height || crop_width == 0 || crop_height == 0 {
        return Err("浏览器截图区域超出当前视口".to_string());
    }
    let cropped = image.crop_imm(left, top, crop_width, crop_height);
    let mut output = Vec::new();
    match format {
        BrowserScreenshotFormat::Png => cropped
            .write_to(&mut Cursor::new(&mut output), image::ImageFormat::Png)
            .map_err(|error| format!("编码 PNG 浏览器截图失败: {error}"))?,
        BrowserScreenshotFormat::Jpeg => {
            JpegEncoder::new_with_quality(&mut output, quality.unwrap_or(80))
                .encode_image(&cropped)
                .map_err(|error| format!("编码 JPEG 浏览器截图失败: {error}"))?
        }
        BrowserScreenshotFormat::Webp => WebPEncoder::new_lossless(&mut output)
            .write_image(
                cropped.as_bytes(),
                cropped.width(),
                cropped.height(),
                cropped.color().into(),
            )
            .map_err(|error| format!("编码 WebP 浏览器截图失败: {error}"))?,
    }
    Ok(output)
}

fn image_format(format: BrowserScreenshotFormat) -> image::ImageFormat {
    match format {
        BrowserScreenshotFormat::Png => image::ImageFormat::Png,
        BrowserScreenshotFormat::Jpeg => image::ImageFormat::Jpeg,
        BrowserScreenshotFormat::Webp => image::ImageFormat::WebP,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_crop_has_deterministic_pixel_size_for_every_format() {
        let source = image::DynamicImage::new_rgb8(200, 100);
        for (format, image_format) in [
            (BrowserScreenshotFormat::Png, image::ImageFormat::Png),
            (BrowserScreenshotFormat::Jpeg, image::ImageFormat::Jpeg),
            (BrowserScreenshotFormat::Webp, image::ImageFormat::WebP),
        ] {
            let mut encoded = Cursor::new(Vec::new());
            source
                .write_to(&mut encoded, image_format)
                .expect("fixture image should encode");
            let cropped = crop_browser_screenshot(
                &encoded.into_inner(),
                format,
                BrowserNormalizedRect {
                    x: 0.25,
                    y: 0.2,
                    width: 0.5,
                    height: 0.4,
                },
                Some(75),
            )
            .expect("normalized crop should succeed");
            let decoded = image::load_from_memory_with_format(&cropped, image_format)
                .expect("cropped image should decode");
            assert_eq!((decoded.width(), decoded.height()), (100, 40));
        }
    }
}

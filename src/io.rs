//! Loading and saving. Flat formats go through the `image` crate (limits
//! removed so huge images open); layered documents use OpenRaster (.ora),
//! Pinta's native format.

use crate::blend::BlendMode;
use crate::document::{Document, Layer};
use crate::surface::{Surface, premul, unpremul};
use rayon::prelude::*;
use std::io::{Read, Write};
use std::path::Path;

pub const OPEN_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "bmp", "gif", "ico", "tif", "tiff", "tga", "webp", "pnm", "ppm", "pgm", "qoi", "exr", "hdr", "ora"];
pub const SAVE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "bmp", "tif", "tiff", "tga", "webp", "ico", "qoi", "ora"];

fn premul_in_place(data: &mut [u8]) {
    data.par_chunks_mut(4 * 4096).for_each(|ch| {
        for p in ch.chunks_exact_mut(4) {
            let c = premul([p[0], p[1], p[2], p[3]]);
            p.copy_from_slice(&c);
        }
    });
}

fn unpremul_in_place(data: &mut [u8]) {
    data.par_chunks_mut(4 * 4096).for_each(|ch| {
        for p in ch.chunks_exact_mut(4) {
            let c = unpremul([p[0], p[1], p[2], p[3]]);
            p.copy_from_slice(&c);
        }
    });
}

pub fn decode_image_bytes(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut r = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string())?;
    r.no_limits();
    let img = r.decode().map_err(|e| e.to_string())?;
    let rgba = img.into_rgba8();
    let (w, h) = rgba.dimensions();
    Ok((w, h, rgba.into_raw()))
}

pub fn surface_from_straight(w: u32, h: u32, mut data: Vec<u8>) -> Surface {
    premul_in_place(&mut data);
    Surface::from_rgba(w, h, &data)
}

pub fn load(path: &Path) -> Result<Document, String> {
    let title = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Image".into());
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mut doc = if ext == "ora" {
        load_ora(path, &title)?
    } else {
        let mut r = image::ImageReader::open(path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?;
        r.no_limits();
        let img = r.decode().map_err(|e| format!("Could not open {}: {e}", path.display()))?;
        let rgba = img.into_rgba8();
        let (w, h) = rgba.dimensions();
        let s = surface_from_straight(w, h, rgba.into_raw());
        Document::from_layers(title, w, h, vec![Layer::new("Background", s)], "Open Image")
    };
    doc.path = Some(path.to_path_buf());
    Ok(doc)
}

pub fn flatten_rgba(doc: &Document) -> Vec<u8> {
    let flat = crate::tools::flatten(doc);
    let mut data = flat.to_rgba();
    unpremul_in_place(&mut data);
    data
}

pub fn save(doc: &Document, path: &Path, jpeg_quality: u8) -> Result<(), String> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if ext == "ora" {
        return save_ora(doc, path);
    }
    let (w, h) = (doc.state.width, doc.state.height);
    let data = flatten_rgba(doc);
    let img = image::RgbaImage::from_raw(w, h, data).ok_or("buffer size mismatch")?;
    match ext.as_str() {
        "jpg" | "jpeg" => {
            // JPEG has no alpha: composite over white like most editors.
            let rgb = image::DynamicImage::ImageRgba8(img).into_rgb8();
            let f = std::fs::File::create(path).map_err(|e| e.to_string())?;
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(f), jpeg_quality);
            enc.encode_image(&rgb).map_err(|e| e.to_string())
        }
        "bmp" | "tga" => image::DynamicImage::ImageRgba8(img).save(path).map_err(|e| e.to_string()),
        _ => img.save(path).map_err(|e| e.to_string()),
    }
}

fn png_bytes(w: u32, h: u32, data: Vec<u8>) -> Result<Vec<u8>, String> {
    let img = image::RgbaImage::from_raw(w, h, data).ok_or("buffer size mismatch")?;
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(out)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn save_ora(doc: &Document, path: &Path) -> Result<(), String> {
    let st = &doc.state;
    let f = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut z = zip::ZipWriter::new(std::io::BufWriter::new(f));
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored).large_file(true);
    z.start_file("mimetype", stored).map_err(|e| e.to_string())?;
    z.write_all(b"image/openraster").map_err(|e| e.to_string())?;
    let mut xml = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<image w=\"{}\" h=\"{}\">\n<stack opacity=\"1\" name=\"root\">\n", st.width, st.height);
    // PNG encoding per layer in parallel
    let pngs: Vec<Result<Vec<u8>, String>> = st
        .layers
        .par_iter()
        .map(|l| {
            let mut d = l.surface.to_rgba();
            unpremul_in_place(&mut d);
            png_bytes(st.width, st.height, d)
        })
        .collect();
    for (i, (l, png)) in st.layers.iter().zip(pngs).enumerate().rev() {
        let name = format!("data/layer{i}.png");
        z.start_file(&name, deflated).map_err(|e| e.to_string())?;
        z.write_all(&png?).map_err(|e| e.to_string())?;
        xml += &format!(
            "<layer opacity=\"{:.2}\" name=\"{}\" visibility=\"{}\" src=\"{}\" composite-op=\"{}\" x=\"0\" y=\"0\" />\n",
            l.opacity,
            xml_escape(&l.name),
            if l.hidden { "hidden" } else { "visible" },
            name,
            l.blend.ora_name()
        );
    }
    xml += "</stack>\n</image>\n";
    z.start_file("stack.xml", deflated).map_err(|e| e.to_string())?;
    z.write_all(xml.as_bytes()).map_err(|e| e.to_string())?;
    // merged image + thumbnail
    let flat = flatten_rgba(doc);
    let img = image::RgbaImage::from_raw(st.width, st.height, flat).ok_or("size")?;
    let thumb = image::imageops::thumbnail(&img, (256 * st.width / st.width.max(st.height)).max(1), (256 * st.height / st.width.max(st.height)).max(1));
    let mut tb = Vec::new();
    image::DynamicImage::ImageRgba8(thumb).write_to(&mut std::io::Cursor::new(&mut tb), image::ImageFormat::Png).map_err(|e| e.to_string())?;
    z.start_file("Thumbnails/thumbnail.png", deflated).map_err(|e| e.to_string())?;
    z.write_all(&tb).map_err(|e| e.to_string())?;
    let mut mb = Vec::new();
    image::DynamicImage::ImageRgba8(img).write_to(&mut std::io::Cursor::new(&mut mb), image::ImageFormat::Png).map_err(|e| e.to_string())?;
    z.start_file("mergedimage.png", deflated).map_err(|e| e.to_string())?;
    z.write_all(&mb).map_err(|e| e.to_string())?;
    z.finish().map_err(|e| e.to_string())?;
    Ok(())
}

fn attr(tag: &quick_xml::events::BytesStart, name: &str) -> Option<String> {
    tag.attributes().flatten().find(|a| a.key.as_ref() == name).map(|a| a.value.to_string())
}

pub fn load_ora(path: &Path, title: &str) -> Result<Document, String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut z = zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|e| e.to_string())?;
    let mut xml = String::new();
    z.by_name("stack.xml").map_err(|e| e.to_string())?.read_to_string(&mut xml).map_err(|e| e.to_string())?;
    let mut reader = quick_xml::Reader::from_str(&xml);
    let mut w = 0u32;
    let mut h = 0u32;
    struct L {
        name: String,
        src: String,
        opacity: f32,
        hidden: bool,
        blend: BlendMode,
        x: i32,
        y: i32,
    }
    let mut ls: Vec<L> = Vec::new();
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Start(t)) | Ok(quick_xml::events::Event::Empty(t)) => match t.name().as_ref() {
                "image" => {
                    w = attr(&t, "w").and_then(|v| v.parse().ok()).unwrap_or(0);
                    h = attr(&t, "h").and_then(|v| v.parse().ok()).unwrap_or(0);
                }
                "layer" => ls.push(L {
                    name: attr(&t, "name").unwrap_or_else(|| "Layer".into()),
                    src: attr(&t, "src").unwrap_or_default(),
                    opacity: attr(&t, "opacity").and_then(|v| v.parse().ok()).unwrap_or(1.0),
                    hidden: attr(&t, "visibility").as_deref() == Some("hidden"),
                    blend: attr(&t, "composite-op").map(|s| BlendMode::from_ora(&s)).unwrap_or_default(),
                    x: attr(&t, "x").and_then(|v| v.parse().ok()).unwrap_or(0),
                    y: attr(&t, "y").and_then(|v| v.parse().ok()).unwrap_or(0),
                }),
                _ => {}
            },
            Ok(quick_xml::events::Event::Eof) => break,
            Err(e) => return Err(e.to_string()),
            _ => {}
        }
    }
    if w == 0 || h == 0 {
        return Err("Invalid OpenRaster file (missing size)".into());
    }
    let mut layers = Vec::new();
    // stack.xml lists top layer first
    for l in ls.into_iter().rev() {
        let mut bytes = Vec::new();
        z.by_name(&l.src).map_err(|e| format!("{}: {e}", l.src))?.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        let (lw, lh, mut data) = decode_image_bytes(&bytes)?;
        premul_in_place(&mut data);
        let mut s = Surface::new(w, h);
        s.write_rect(crate::surface::IRect::new(l.x, l.y, lw as i32, lh as i32), &data);
        s.compact();
        let mut layer = Layer::new(l.name, s);
        layer.opacity = l.opacity;
        layer.hidden = l.hidden;
        layer.blend = l.blend;
        layers.push(layer);
    }
    if layers.is_empty() {
        layers.push(Layer::new("Background", Surface::new(w, h)));
    }
    Ok(Document::from_layers(title, w, h, layers, "Open Image"))
}

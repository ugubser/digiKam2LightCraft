//! Versioned translation into LightCraft's model. The bundle remains target independent.
use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use clap::ValueEnum;
use lightcraft_engine::{
    catalog::{ColorLabel, Flag, MediaKind, Photo, PhotoId, Source},
    crs, sidecar,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model::{ADAPTER, MAX_PACKET, Packet, Record, integer, number, text};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Faces {
    None,
    #[default]
    Named,
    All,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum MetadataSource {
    #[default]
    Database,
    Sidecar,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Options {
    pub faces: Faces,
    pub metadata_source: MetadataSource,
    #[serde(default)]
    pub roots: Vec<crate::model::RootMapping>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Notice {
    pub source_id: i64,
    pub code: String,
    pub detail: String,
}

pub fn capabilities() -> Value {
    json!({"adapter":ADAPTER,"bundleVersion":1,"referenceOriginals":true,"writeSidecars":false,
        "native":["ratings","pick/reject","five color labels","hierarchical keywords","captions/title","camera/lens/exposure","capture dates","GPS coordinates","copyright/creator","named face regions","stacks"],
        "approximate":["foreign XMP develop settings","Magenta to Purple"],
        "archived":["Pending pick state","extra color labels","GPS altitude/accuracy","full IPTC contacts","digiKam edit history","saved searches","similarity search definitions","automatic face regions by default","unknown database fields"],
        "video":"catalog records and metadata; no playback/trim/export",
        "heif":"records retained; rendering requires a LightCraft build with HEIF enabled",
        "upgradePolicy":"per-field three-way comparison; changed LightCraft values are never overwritten"})
}

pub fn packet_bytes(packet: &Packet) -> Result<Vec<u8>> {
    ensure!(
        packet.body.len() as u64 <= MAX_PACKET.saturating_mul(2),
        "encoded XMP exceeds packet limit"
    );
    let bytes = match packet.encoding.as_str() {
        "utf8" => packet.body.as_bytes().to_vec(),
        "base64" => STANDARD
            .decode(&packet.body)
            .context("invalid archived XMP encoding")?,
        _ => anyhow::bail!("unsupported archived XMP encoding"),
    };
    ensure!(
        bytes.len() as u64 <= MAX_PACKET,
        "decoded XMP exceeds packet limit"
    );
    Ok(bytes)
}

pub fn map(
    record: &Record,
    id: PhotoId,
    now: &str,
    options: &Options,
) -> Result<(Photo, Vec<Notice>)> {
    let mut notices = Vec::new();
    let mut note = |code: &str, detail: String| {
        notices.push(Notice {
            source_id: record.source_id,
            code: code.to_string(),
            detail,
        })
    };
    let info = record.row("ImageInformation");
    let image = record.row("Images");
    let source_format = text(info, "format");
    let format = source_format.strip_prefix("RAW-").unwrap_or(&source_format);
    let orientation = integer(info, "orientation").unwrap_or(1);
    let dim = |key| {
        integer(info, key)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0)
    };
    let (w, h) = (dim("width"), dim("height"));
    let (width, height) = if (5..=8).contains(&orientation) {
        (h, w)
    } else {
        (w, h)
    };
    let name = text(image, "name");
    let path = record.path.to_str().context("photo path is not UTF-8")?;
    let mut photo = Photo::new(
        id,
        Source::File {
            path: path.to_string(),
        },
        &name,
        format,
        width,
        height,
        now,
    );
    photo.kind = if integer(image, "category") == Some(2) {
        MediaKind::Video
    } else if source_format.starts_with("RAW-") {
        MediaKind::Raw
    } else {
        MediaKind::Image
    };
    photo.file_size = integer(image, "fileSize")
        .and_then(|n| u64::try_from(n).ok())
        .unwrap_or(0);
    let captured = text(info, "creationDate");
    photo.captured = (!captured.is_empty()).then_some(captured);
    let rating = integer(info, "rating").unwrap_or(-1);
    if !(-1..=5).contains(&rating) {
        note(
            "invalid-rating",
            format!("database rating {rating} retained only in archive"),
        );
    }
    photo.rating = u8::try_from(rating.clamp(0, 5)).unwrap_or(0);
    let md = record.row("ImageMetadata");
    photo.meta.camera = [text(md, "make"), text(md, "model")]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    photo.meta.lens = text(md, "lens");
    photo.meta.aperture = number(md, "aperture").and_then(positive_f32);
    photo.meta.focal_mm = number(md, "focalLength").and_then(positive_f32);
    photo.meta.iso = integer(md, "sensitivity")
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0);
    if let Some(t) = number(md, "exposureTime").filter(|n| *n > 0.0) {
        let reciprocal = t.recip();
        photo.meta.shutter = if t < 1.0
            && reciprocal.is_finite()
            && (reciprocal - reciprocal.round()).abs() <= reciprocal * 0.0001
        {
            format!("1/{:.0}", reciprocal.round())
        } else {
            format!("{t}")
        };
    }
    let pos = record.row("ImagePositions");
    if let (Some(lat), Some(lon)) = (
        number(pos, "latitudeNumber"),
        number(pos, "longitudeNumber"),
    ) {
        if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) {
            photo.meta.gps = Some((lat, lon));
        } else {
            note(
                "invalid-gps",
                "out-of-range coordinates retained in archive".to_string(),
            );
        }
    }
    for row in record.rows("ImageProperties") {
        let value = text(Some(row), "value");
        match text(Some(row), "property").as_str() {
            "country" => photo.meta.country = value,
            "city" => photo.meta.city = value,
            "provinceState" => photo.meta.state = value,
            "location" => photo.meta.location = value,
            _ => {}
        }
    }
    for ty in [1, 3] {
        let mut comments = record
            .rows("ImageComments")
            .filter(|r| integer(Some(r), "type") == Some(ty))
            .collect::<Vec<_>>();
        comments.sort_by_key(|r| {
            (
                text(Some(r), "language") != "x-default",
                integer(Some(r), "id").unwrap_or(0),
            )
        });
        if let Some(first) = comments.first() {
            let value = text(Some(first), "comment");
            if ty == 1 {
                photo.meta.caption = value;
            } else {
                photo.meta.title = value;
            }
        }
        if comments.len() > 1 {
            note(
                "additional-comments-archived",
                format!(
                    "{} comments of type {ty}; preferred x-default for native field",
                    comments.len()
                ),
            );
        }
    }
    for row in record.rows("ImageCopyright") {
        let value = text(Some(row), "value");
        if value.is_empty() {
            continue;
        }
        match text(Some(row), "property").as_str() {
            "copyrightNotice" => photo.meta.copyright = value,
            "creator" => photo.meta.creator = value,
            property => note("iptc-field-archived", property.to_string()),
        }
    }
    for tag in &record.tags {
        if tag.properties.contains_key("internalTag") {
            match tag.name.as_str() {
                "Pick Label Accepted" => photo.flag = Flag::Pick,
                "Pick Label Rejected" => photo.flag = Flag::Reject,
                "Pick Label Pending" => {
                    photo.meta.keywords.push("digiKam|Pending".to_string());
                    note(
                        "pending-pick",
                        "preserved as digiKam|Pending keyword".to_string(),
                    );
                }
                "Color Label Magenta" => {
                    photo.label = Some(ColorLabel::Purple);
                    note(
                        "magenta-to-purple",
                        "native Purple; original Magenta retained in source".to_string(),
                    );
                }
                name if name.starts_with("Color Label ") => {
                    let label = name.trim_start_matches("Color Label ");
                    if let Some(label) = ColorLabel::parse(label) {
                        photo.label = Some(label);
                    } else if label != "None" {
                        photo
                            .meta
                            .keywords
                            .push(format!("digiKam|Color Label {label}"));
                        note("color-label-archived", label.to_string());
                    }
                }
                _ => {}
            }
        } else if !["unknownPerson", "unconfirmedPerson", "ignoredPerson"]
            .iter()
            .any(|p| tag.properties.contains_key(*p))
        {
            photo.meta.keywords.push(tag.path.clone());
        }
    }
    photo.duration = record
        .row("VideoMetadata")
        .and_then(|r| r.get("duration"))
        .and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .filter(|n| n.is_finite() && *n >= 0.0)
        .map(|milliseconds| milliseconds / 1000.0);
    if photo.kind == MediaKind::Video {
        note(
            "video-catalog-only",
            "video metadata retained; playback is not supported by this adapter".to_string(),
        );
    }
    if format == "HEIF" || format == "HEIC" {
        note(
            "heif-build-required",
            "rendering requires a LightCraft build with HEIF enabled".to_string(),
        );
    }
    if !record.present {
        note("missing-original", path.to_string());
    }
    // A digiKam uniqueHash is not LightCraft's byte hash. Leave content_hash unset.
    photo.develop = Arc::new(photo.import_defaults());
    let database_meta = photo.meta.clone();
    let database_rating = photo.rating;
    let database_flag = photo.flag;
    let database_label = photo.label;
    // Full-name sidecars are selected ahead of shared stem sidecars. Both packets remain in
    // the bundle, but combining their adjustments could apply two different edits to one file.
    if let Some(packet) = record.sidecars.iter().find(|p| p.applicable) {
        let bytes = packet_bytes(packet)?;
        match std::str::from_utf8(&bytes).ok().and_then(|xml| {
            sidecar::parse_sidecar(xml, crs::Target::for_photo(&photo))
                .ok()
                .map(|sc| (xml, sc))
        }) {
            Some((xml, sc)) => {
                if let Some(r) = sc.rating
                    && r != database_rating
                {
                    note(
                        "database-xmp-rating",
                        format!(
                            "database={database_rating}, sidecar={r}; precedence={:?}",
                            options.metadata_source
                        ),
                    );
                }
                for w in &sc.warnings {
                    note("xmp-unmapped", format!("{w:?}"));
                }
                sidecar::merge_into(&mut photo, &sc, now);
                // digiKam's unrotated MWG rectangles are sensor-relative when it writes a TIFF
                // orientation. Verified against matching database and MWG rectangles with EXIF orientation 8. Explicit MWG rotations are already handled by LightCraft.
                let doc = roxmltree::Document::parse(xml).ok();
                let has_rotation = doc.as_ref().is_some_and(|d| {
                    d.descendants().any(|n| {
                        n.has_tag_name((
                            "http://www.metadataworkinggroup.com/schemas/regions/",
                            "Rotation",
                        )) || n.attributes().any(|a| {
                            a.namespace()
                                == Some("http://www.metadataworkinggroup.com/schemas/regions/")
                                && a.name() == "Rotation"
                        })
                    })
                });
                if !has_rotation {
                    let tiff_orientation = doc
                        .as_ref()
                        .and_then(|d| {
                            d.descendants().find_map(|n| {
                                n.attributes()
                                    .find(|a| {
                                        a.namespace() == Some("http://ns.adobe.com/tiff/1.0/")
                                            && a.name() == "Orientation"
                                    })
                                    .and_then(|a| a.value().parse::<i64>().ok())
                                    .or_else(|| {
                                        (n.tag_name().namespace()
                                            == Some("http://ns.adobe.com/tiff/1.0/")
                                            && n.tag_name().name() == "Orientation")
                                            .then(|| n.text().and_then(|s| s.parse().ok()))
                                            .flatten()
                                    })
                            })
                        })
                        .unwrap_or(1);
                    for region in &mut photo.meta.regions {
                        let r = region.rect;
                        let r = orient_rect([r.x0, r.y0, r.x1, r.y1], tiff_orientation);
                        region.rect.x0 = r[0];
                        region.rect.y0 = r[1];
                        region.rect.x1 = r[2];
                        region.rect.y1 = r[3];
                    }
                }
                if options.metadata_source == MetadataSource::Database {
                    photo.rating = database_rating;
                    // An absent database label/flag can be filled by XMP. Explicit source values win.
                    if record.tags.iter().any(|t| {
                        t.properties.contains_key("internalTag")
                            && t.name.starts_with("Pick Label ")
                    }) {
                        photo.flag = database_flag;
                    }
                    if record.tags.iter().any(|t| {
                        t.properties.contains_key("internalTag")
                            && t.name.starts_with("Color Label ")
                    }) {
                        photo.label = database_label;
                    }
                    fill_preferred_metadata(&mut photo.meta, &database_meta);
                }
            }
            None => note(
                "unreadable-xmp",
                format!("preserved verbatim: {}", packet.path.display()),
            ),
        }
        if record.sidecars.len() > 1 {
            note(
                "multiple-sidecars",
                "selected full-name packet first; other packets archived".to_string(),
            );
        }
    }
    if options.metadata_source == MetadataSource::Sidecar {
        // Keywords are additive: an XMP edit must not erase database organization.
        photo.meta.keywords.extend(database_meta.keywords);
    }
    if options.faces != Faces::None {
        for row in record.rows("ImageTagProperties") {
            let property = text(Some(row), "property");
            if property != "tagRegion" && property != "autodetectedFace" {
                continue;
            }
            let tag =
                integer(Some(row), "tagid").and_then(|id| record.tags.iter().find(|t| t.id == id));
            let named = property == "tagRegion"
                && tag.is_some_and(|t| {
                    !["unknownPerson", "unconfirmedPerson", "ignoredPerson"]
                        .iter()
                        .any(|p| t.properties.contains_key(*p))
                });
            if !named && options.faces != Faces::All {
                continue;
            }
            if let Some(rect) = database_rect(&text(Some(row), "value"), width, height) {
                let name = named.then(|| tag.map(|t| t.name.clone())).flatten();
                let region = serde_json::from_value(
                    json!({"rect":{"x0":rect[0],"y0":rect[1],"x1":rect[2],"y1":rect[3]},"kind":"Face","name":name,"description":if named { None::<String> } else { Some("digiKam automatic detection; unconfirmed".to_string()) }}),
                )?;
                if !photo.meta.regions.iter().any(|r| {
                    let b = r.rect;
                    r.name == name
                        && [
                            b.x0 - rect[0],
                            b.y0 - rect[1],
                            b.x1 - rect[2],
                            b.y1 - rect[3],
                        ]
                        .iter()
                        .all(|v| v.abs() < 0.002)
                }) {
                    photo.meta.regions.push(region);
                }
            } else {
                note(
                    "invalid-face-region",
                    "coordinates missing or outside oriented frame; retained in bundle".to_string(),
                );
            }
        }
    }
    match options.faces {
        Faces::None => photo.meta.regions.clear(),
        Faces::Named => photo
            .meta
            .regions
            .retain(|r| r.name.as_ref().is_some_and(|n| !n.trim().is_empty())),
        Faces::All => {}
    }
    if record.sidecars.iter().any(|p| !p.applicable) {
        note("shared-stem-sidecar", "stem packet belongs to another file, usually the raw in a RAW/JPEG pair; archived without applying".to_string());
    }
    photo.meta.keywords.sort();
    photo.meta.keywords.dedup();
    Ok((photo, notices))
}

fn fill_preferred_metadata(
    target: &mut lightcraft_engine::catalog::Meta,
    source: &lightcraft_engine::catalog::Meta,
) {
    for (dst, src) in [
        (&mut target.title, &source.title),
        (&mut target.caption, &source.caption),
        (&mut target.copyright, &source.copyright),
        (&mut target.creator, &source.creator),
        (&mut target.location, &source.location),
        (&mut target.city, &source.city),
        (&mut target.state, &source.state),
        (&mut target.country, &source.country),
    ] {
        if !src.is_empty() {
            *dst = src.clone();
        }
    }
    if source.gps.is_some() {
        target.gps = source.gps;
    }
    target.keywords.extend(source.keywords.clone());
}

pub fn database_rect(xml: &str, width: u32, height: u32) -> Option<[f64; 4]> {
    if width == 0 || height == 0 {
        return None;
    }
    let doc = roxmltree::Document::parse(xml).ok()?;
    let node = doc.root_element();
    if node.tag_name().name() != "rect" {
        return None;
    }
    let n = |name| {
        node.attribute(name)?
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
    };
    let (x, y, w, h) = (n("x")?, n("y")?, n("width")?, n("height")?);
    if x < 0.0 || y < 0.0 || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let rect = [
        x / f64::from(width),
        y / f64::from(height),
        (x + w) / f64::from(width),
        (y + h) / f64::from(height),
    ];
    if rect.iter().any(|n| !n.is_finite()) || rect[2] > 1.01 || rect[3] > 1.01 {
        return None;
    }
    Some(rect.map(|n| n.clamp(0.0, 1.0)))
}

pub fn orient_rect(rect: [f64; 4], orientation: i64) -> [f64; 4] {
    let point = |x: f64, y: f64| match orientation {
        2 => (1.0 - x, y),
        3 => (1.0 - x, 1.0 - y),
        4 => (x, 1.0 - y),
        5 => (y, x),
        6 => (1.0 - y, x),
        7 => (1.0 - y, 1.0 - x),
        8 => (y, 1.0 - x),
        _ => (x, y),
    };
    let a = point(rect[0], rect[1]);
    let b = point(rect[2], rect[3]);
    [a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)]
}

/// Independently guarded field groups for replay. Regions, keywords and develop are separate so
/// a rating change does not prevent a future adapter from importing new face support.
pub fn groups(photo: &Photo) -> Result<BTreeMap<String, Value>> {
    let mut v = serde_json::to_value(photo)?;
    let obj = v.as_object_mut().context("photo is not an object")?;
    let mut groups = BTreeMap::new();
    for name in [
        "rating",
        "flag",
        "label",
        "captured",
        "duration",
        "develop",
        "source",
        "file_name",
    ] {
        groups.insert(name.to_string(), obj.remove(name).unwrap_or(Value::Null));
    }
    let mut meta = obj.remove("meta").unwrap_or(json!({}));
    if let Some(obj) = meta.as_object_mut() {
        groups.insert(
            "regions".to_string(),
            obj.remove("regions").unwrap_or(json!([])),
        );
        groups.insert(
            "keywords".to_string(),
            obj.remove("keywords").unwrap_or(json!([])),
        );
    }
    groups.insert("meta".to_string(), meta);
    groups.insert("technical".to_string(),json!({"width":photo.width,"height":photo.height,"file_size":photo.file_size,"kind":photo.kind,"format":photo.format}));
    Ok(groups)
}

pub fn replace_groups(photo: &Photo, groups: &BTreeMap<String, Value>) -> Result<Photo> {
    let mut value = serde_json::to_value(photo)?;
    let obj = value.as_object_mut().context("photo is not an object")?;
    for (name, val) in groups {
        match name.as_str() {
            "regions" | "keywords" => {
                obj.get_mut("meta")
                    .and_then(Value::as_object_mut)
                    .context("photo has no meta")?
                    .insert(name.clone(), val.clone());
            }
            "meta" => {
                let current = obj.get_mut("meta").context("photo has no meta")?;
                let region = current.get("regions").cloned();
                let keywords = current.get("keywords").cloned();
                *current = val.clone();
                let o = current
                    .as_object_mut()
                    .context("mapped metadata is not an object")?;
                if let Some(v) = region {
                    o.insert("regions".to_string(), v);
                }
                if let Some(v) = keywords {
                    o.insert("keywords".to_string(), v);
                }
            }
            "technical" => {
                for (k, v) in val.as_object().context("invalid technical group")? {
                    obj.insert(k.clone(), v.clone());
                }
            }
            _ => {
                obj.insert(name.clone(), val.clone());
            }
        }
    }
    Ok(serde_json::from_value(value)?)
}

fn positive_f32(n: f64) -> Option<f32> {
    let value = n as f32;
    (value.is_finite() && value > 0.0).then_some(value)
}

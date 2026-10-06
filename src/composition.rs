//! Assemble already annotated pages without inspecting or comparing their text.
//! Each original page becomes a PDF form at its original dimensions. Its fonts,
//! images, and vector artwork remain intact; a missing counterpart stays blank.

use anyhow::{Context, Result, ensure};
use lopdf::{
    Document, Object, ObjectId, Stream,
    content::{Content, Operation},
    dictionary,
};
use std::{path::Path, time::Instant};

struct Page {
    form: ObjectId,
    width: f32,
    height: f32,
}

fn inherited(document: &Document, mut page: ObjectId, key: &[u8]) -> Result<Option<Object>> {
    for _ in 0..64 {
        let dictionary = document.get_object(page)?.as_dict()?;
        if let Ok(value) = dictionary.get(key) {
            return Ok(Some(document.dereference(value)?.1.clone()));
        }
        let Ok(parent) = dictionary.get(b"Parent") else {
            return Ok(None);
        };
        page = parent.as_reference()?;
    }
    anyhow::bail!("PDF page inheritance exceeds 64 levels")
}

fn import(path: Option<&Path>, output: &mut Document, deadline: Instant) -> Result<Vec<Page>> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let mut source = Document::load(path)
        .with_context(|| format!("reading pages for assembly: {}", path.display()))?;
    source.renumber_objects_with(output.max_id + 1);
    output.max_id = source.max_id;
    let mut pages = Vec::new();
    for page in source.get_pages().into_values() {
        ensure!(
            Instant::now() < deadline,
            "PDF page assembly exceeded its timeout"
        );
        let bounds = inherited(&source, page, b"CropBox")?
            .or(inherited(&source, page, b"MediaBox")?)
            .context("PDF page has no dimensions")?;
        let numbers = bounds
            .as_array()?
            .iter()
            .map(Object::as_float)
            .collect::<lopdf::Result<Vec<_>>>()?;
        ensure!(numbers.len() == 4, "invalid PDF page dimensions");
        let (x0, y0, x1, y1) = (numbers[0], numbers[1], numbers[2], numbers[3]);
        let rotation = inherited(&source, page, b"Rotate")?
            .map_or(Ok(0), |v| v.as_i64())?
            .rem_euclid(360);
        let unit = inherited(&source, page, b"UserUnit")?.map_or(Ok(1.0), |v| v.as_float())?;
        let (mut width, mut height) = ((x1 - x0) * unit, (y1 - y0) * unit);
        ensure!(
            width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0,
            "invalid PDF page size"
        );
        let matrix = match rotation {
            0 => [unit, 0.0, 0.0, unit, -x0 * unit, -y0 * unit],
            90 => [0.0, -unit, unit, 0.0, -y0 * unit, x1 * unit],
            180 => [-unit, 0.0, 0.0, -unit, x1 * unit, y1 * unit],
            270 => [0.0, unit, -unit, 0.0, y1 * unit, -x0 * unit],
            _ => anyhow::bail!("PDF page rotation must be a multiple of 90 degrees"),
        };
        if rotation == 90 || rotation == 270 {
            std::mem::swap(&mut width, &mut height);
        }
        let resources =
            inherited(&source, page, b"Resources")?.unwrap_or_else(|| dictionary! {}.into());
        let mut dictionary = dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "FormType" => 1,
            "BBox" => bounds,
            "Resources" => resources,
            "Matrix" => Object::Array(matrix.into_iter().map(Object::Real).collect())
        };
        if let Some(group) = inherited(&source, page, b"Group")? {
            dictionary.set("Group", group);
        }
        let mut form = Stream::new(dictionary, source.get_page_content(page)?);
        form.compress()?;
        pages.push(Page {
            form: output.add_object(form),
            width,
            height,
        });
    }
    output.objects.extend(source.objects);
    Ok(pages)
}

/// Pair page i of each version. This function is composition only: the LaTeX
/// comparison has already decided every highlight before these PDFs exist.
pub fn pair(
    old: Option<&Path>,
    new: Option<&Path>,
    destination: &Path,
    old_label: &str,
    new_label: &str,
    deadline: Instant,
) -> Result<(usize, usize)> {
    let mut output = Document::with_version("1.7");
    let parent = output.new_object_id();
    let old = import(old, &mut output, deadline)?;
    let new = import(new, &mut output, deadline)?;
    let font = output.add_object(
        dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" },
    );
    let mut kids = Vec::new();
    for index in 0..old.len().max(new.len()) {
        ensure!(
            Instant::now() < deadline,
            "PDF page assembly exceeded its timeout"
        );
        let left = old.get(index);
        let right = new.get(index);
        let left_size = left
            .or(old.first())
            .or(right)
            .context("empty PDF page pair")?;
        let right_size = right
            .or(new.first())
            .or(left)
            .context("empty PDF page pair")?;
        let width = left_size.width + 12.0 + right_size.width;
        let height = left_size.height.max(right_size.height);
        let mut xobjects = dictionary! {};
        let mut operations = Vec::new();
        for (name, page, x) in [("Old", left, 0.0), ("New", right, left_size.width + 12.0)] {
            if let Some(page) = page {
                xobjects.set(name, page.form);
                operations.extend([
                    Operation::new("q", vec![]),
                    Operation::new(
                        "cm",
                        vec![
                            1.into(),
                            0.into(),
                            0.into(),
                            1.into(),
                            Object::Real(x),
                            Object::Real(height - page.height),
                        ],
                    ),
                    Operation::new("Do", vec![Object::Name(name.as_bytes().to_vec())]),
                    Operation::new("Q", vec![]),
                ]);
            }
        }
        for (label, count, x, rgb) in [
            (old_label, old.len(), 12.0, [0.8, 0.078, 0.078]),
            (
                new_label,
                new.len(),
                left_size.width + 24.0,
                [0.059, 0.2, 0.851],
            ),
        ] {
            let text = if index < count {
                format!("{label} — page {} of {count}", index + 1).replace('—', "-")
            } else {
                format!("{label} - no page")
            };
            operations.extend([
                Operation::new("q", vec![]),
                Operation::new("rg", rgb.into_iter().map(Object::Real).collect()),
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"LabelFont".to_vec()), 9.into()]),
                Operation::new("Td", vec![Object::Real(x), Object::Real(height + 6.0)]),
                Operation::new("Tj", vec![Object::string_literal(text)]),
                Operation::new("ET", vec![]),
                Operation::new("Q", vec![]),
            ]);
        }
        let mut content = Stream::new(dictionary! {}, Content { operations }.encode()?);
        content.compress()?;
        let content = output.add_object(content);
        let page = output.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => parent,
            "MediaBox" => vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Real(width),
                Object::Real(height + 20.0),
            ],
            "Resources" => dictionary! {
                "XObject" => xobjects,
                "Font" => dictionary! { "LabelFont" => font }
            },
            "Contents" => content
        });
        kids.push(Object::Reference(page));
    }
    ensure!(!kids.is_empty(), "neither LaTeX version produced a page");
    output.objects.insert(
        parent,
        dictionary! { "Type" => "Pages", "Count" => kids.len() as i64, "Kids" => kids }.into(),
    );
    let root = output.add_object(dictionary! { "Type" => "Catalog", "Pages" => parent });
    output.trailer.set("Root", root);
    let info = output.add_object(dictionary! {
        "Creator" => Object::string_literal("tex-diff (LaTeX source comparison)"),
        "Title" => Object::string_literal("LaTeX review - before and after")
    });
    output.trailer.set("Info", info);
    output
        .save(destination)
        .context("saving paired review pages")?;
    Ok((old.len(), new.len()))
}

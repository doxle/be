// ─────────────────────────────────────────────────────────────────────────────
// BULK IMPORT FLOW (3-step batch architecture)
// ─────────────────────────────────────────────────────────────────────────────
//
// STEP 1: POST /import/parse
//
//   FE uploads zip to S3 (presigned URL)
//   On success, FE calls POST /import/parse
//                       │
//                  Lambda downloads zip from S3
//                  Unzip in memory
//                  Count: images, labels, tasks, annotations
//                  Write manifest.json into zip
//                  Re-upload zip to S3
//                       │
//                  ◄── Return manifest to FE
//
// STEP 2: POST /import/process-batch  (called N times)
//
//   FE sends: {phase: "labels", offset: 0, limit: 50}
//                       │
//   Lambda downloads zip from S3
//   Reads manifest.json
//   Processes items [offset..offset+limit]
//                       │
//   ◄── Returns: {processed: 50, total: 180}
//
//   Phases (in order): labels ──► tasks ──► images ──► annotations
//
// STEP 3: POST /import/cleanup
//
//   Delete zip from S3
//   ◄── Return final summary
//
// ─────────────────────────────────────────────────────────────────────────────

use aws_sdk_s3::Client as S3Client;
use aws_sdk_dynamodb::Client as DynamoClient;
use lambda_http::{Body, Error, Response, http::StatusCode};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read, Write};
use zip::ZipArchive;
use zip::write::{FileOptions, ZipWriter};

fn get_bucket_name() -> String {
    std::env::var("S3_BUCKET_NAME").unwrap_or_else(|_| "doxle-app".to_string())
}

fn merge_xml_import_datasets(datasets: Vec<XmlImportDataset>) -> XmlImportDataset {
    let mut labels: Vec<String> = Vec::new();
    let mut label_seen: HashSet<String> = HashSet::new();
    let mut tasks: Vec<XmlTaskSpec> = Vec::new();
    let mut task_seen: HashSet<String> = HashSet::new();
    let mut images: Vec<XmlImageEntry> = Vec::new();

    for dataset in datasets {
        let mut task_key_remap: HashMap<String, String> = HashMap::new();

        for task in dataset.tasks {
            let mut resolved_key = task.key.clone();
            if task_seen.contains(&resolved_key) {
                if let Some(existing) = tasks.iter().find(|t| t.key == resolved_key) {
                    if existing.task_name == task.task_name && existing.subset == task.subset {
                        task_key_remap.insert(task.key.clone(), resolved_key);
                        continue;
                    }
                }

                let mut suffix = 1usize;
                loop {
                    let candidate = format!("{}::{}", task.key, suffix);
                    if !task_seen.contains(&candidate) {
                        resolved_key = candidate;
                        break;
                    }
                    suffix += 1;
                }
            }

            task_seen.insert(resolved_key.clone());
            task_key_remap.insert(task.key.clone(), resolved_key.clone());
            tasks.push(XmlTaskSpec {
                key: resolved_key,
                task_name: task.task_name,
                subset: task.subset,
            });
        }

        for mut image in dataset.images {
            if let Some(mapped_key) = task_key_remap.get(&image.task_key) {
                image.task_key = mapped_key.clone();
            }
            images.push(image);
        }

        for label in dataset.labels {
            let key = normalize_key(&label);
            if label_seen.insert(key) {
                labels.push(label);
            }
        }
    }

    XmlImportDataset {
        labels,
        tasks,
        images,
    }
}

// ─── Initiate Import (presigned URL for zip upload) ───

#[derive(Deserialize)]
pub struct InitiateImportRequest {
    pub file_name: String,
    pub file_size: usize,
}

#[derive(Serialize)]
pub struct InitiateImportResponse {
    pub import_id: String,
    pub s3_key: String,
    pub upload_url: String,
    pub upload_id: Option<String>,
    pub upload_urls: Vec<ImportUploadPart>,
    pub is_multipart: bool,
}

#[derive(Serialize)]
pub struct ImportUploadPart {
    pub part_number: i32,
    pub upload_url: String,
}

const MULTIPART_THRESHOLD: usize = 50 * 1024 * 1024; // 50MB

/// Generate presigned URL(s) for uploading a CVAT zip to S3
pub async fn initiate_import(
    s3_client: &S3Client,
    block_id: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let request: InitiateImportRequest = serde_json::from_slice(body)?;
    let import_id = uuid::Uuid::new_v4().to_string();
    let s3_key = format!("imports/{}/{}.zip", block_id, import_id);
    let bucket = get_bucket_name();

    let is_multipart = request.file_size >= MULTIPART_THRESHOLD;

    if is_multipart {
        let num_parts = (request.file_size as f64 / MULTIPART_THRESHOLD as f64).ceil() as i32;

        let create_result = s3_client
            .create_multipart_upload()
            .bucket(&bucket)
            .key(&s3_key)
            .content_type("application/zip")
            .send()
            .await
            .map_err(|e| format!("Failed to initiate multipart upload: {}", e))?;

        let upload_id = create_result
            .upload_id()
            .ok_or("No upload ID returned")?
            .to_string();

        let mut upload_parts = Vec::new();
        for part_number in 1..=num_parts {
            let presigned = s3_client
                .upload_part()
                .bucket(&bucket)
                .key(&s3_key)
                .upload_id(&upload_id)
                .part_number(part_number)
                .presigned(
                    aws_sdk_s3::presigning::PresigningConfig::expires_in(
                        std::time::Duration::from_secs(3600),
                    )?,
                )
                .await
                .map_err(|e| format!("Failed to generate presigned URL for part {}: {}", part_number, e))?;

            upload_parts.push(ImportUploadPart {
                part_number,
                upload_url: presigned.uri().to_string(),
            });
        }

        let response = InitiateImportResponse {
            import_id,
            s3_key,
            upload_url: String::new(),
            upload_id: Some(upload_id),
            upload_urls: upload_parts,
            is_multipart: true,
        };

        Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(serde_json::to_string(&response)?.into())
            .map_err(Box::new)?)
    } else {
        let presigned = s3_client
            .put_object()
            .bucket(&bucket)
            .key(&s3_key)
            .content_type("application/zip")
            .presigned(
                aws_sdk_s3::presigning::PresigningConfig::expires_in(
                    std::time::Duration::from_secs(3600),
                )?,
            )
            .await
            .map_err(|e| format!("Failed to generate presigned URL: {}", e))?;

        let response = InitiateImportResponse {
            import_id,
            s3_key: s3_key.clone(),
            upload_url: presigned.uri().to_string(),
            upload_id: None,
            upload_urls: vec![],
            is_multipart: false,
        };

        Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(serde_json::to_string(&response)?.into())
            .map_err(Box::new)?)
    }
}

// ─── Abort Multipart Import Upload ───

#[derive(Deserialize)]
pub struct AbortImportUploadRequest {
    pub s3_key: String,
    pub upload_id: String,
}

/// Abort a multipart upload — cleans up orphaned parts in S3
pub async fn abort_import_upload(
    s3_client: &S3Client,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let request: AbortImportUploadRequest = serde_json::from_slice(body)?;

    s3_client
        .abort_multipart_upload()
        .bucket(&get_bucket_name())
        .key(&request.s3_key)
        .upload_id(&request.upload_id)
        .send()
        .await
        .map_err(|e| format!("Failed to abort multipart upload: {}", e))?;

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(
            serde_json::json!({
                "status": "aborted"
            })
            .to_string()
            .into(),
        )
        .map_err(Box::new)?)
}

// ─── Complete Multipart Import Upload ───

#[derive(Deserialize)]
pub struct CompleteImportUploadRequest {
    pub import_id: String,
    pub s3_key: String,
    pub upload_id: String,
    pub parts: Vec<CompletedImportPart>,
}

#[derive(Deserialize)]
pub struct CompletedImportPart {
    pub part_number: i32,
    pub etag: String,
}

// ─── COCO JSON types ───

#[derive(Deserialize, Debug)]
struct CocoDataset {
    images: Vec<CocoImage>,
    annotations: Vec<CocoAnnotation>,
    categories: Vec<CocoCategory>,
}

#[derive(Deserialize, Debug)]
struct CocoImage {
    id: u64,
    file_name: String,
    width: u32,
    height: u32,
}

#[derive(Deserialize, Debug)]
struct CocoAnnotation {
    #[allow(dead_code)]
    id: u64,
    image_id: u64,
    category_id: u64,
    #[serde(default)]
    bbox: Vec<f64>,          // [x, y, w, h]
    #[serde(default)]
    segmentation: serde_json::Value, // [[x1,y1,x2,y2,...]] or RLE
}

#[derive(Deserialize, Debug)]
struct CocoCategory {
    id: u64,
    name: String,
}

// ─── Process Import Request/Response ───

#[derive(Deserialize)]
pub struct ProcessImportRequest {
    pub import_id: String,
    pub s3_key: String,
}

#[derive(Serialize)]
pub struct ProcessImportResponse {
    pub status: String,
    pub labels_created: usize,
    pub tasks_created: usize,
    pub images_created: usize,
    pub annotations_created: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct XmlTaskSpec {
    key: String,
    task_name: String,
    subset: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct XmlImageAnnotation {
    label_name: String,
    geometry: doxle_atoms::drawing::model::Geometry,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct XmlImageEntry {
    file_name: String,
    task_key: String,
    annotations: Vec<XmlImageAnnotation>,
    #[serde(default)]
    source_s3_key: Option<String>,
    #[serde(default)]
    source_width: Option<u32>,
    #[serde(default)]
    source_height: Option<u32>,
    #[serde(default)]
    source_order: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct XmlImportDataset {
    labels: Vec<String>,
    tasks: Vec<XmlTaskSpec>,
    images: Vec<XmlImageEntry>,
}

// ─── Import Manifest (stored as _manifest.json inside the zip) ───

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ImportManifest {
    format: String, // "cvat_xml" or "coco_json"
    import_id: String,
    dataset: XmlImportDataset,
    total_labels: usize,
    total_tasks: usize,
    total_images: usize,
    total_annotations: usize,
}

#[derive(Deserialize)]
pub struct ParseImportRequest {
    pub import_id: String,
    pub s3_key: String,
    pub max_tasks: Option<usize>,
}

#[derive(Serialize, Deserialize)]
pub struct ParseImportResponse {
    pub format: String,
    pub total_labels: usize,
    pub total_tasks: usize,
    pub total_images: usize,
    pub total_annotations: usize,
}

#[derive(Deserialize)]
pub struct ProcessBatchRequest {
    pub s3_key: String,
    pub phase: String, // "labels", "tasks", "images"
    pub offset: usize,
    pub limit: usize,
}

#[derive(Serialize, Deserialize)]
pub struct ProcessBatchResponse {
    pub phase: String,
    pub processed: usize,
    pub total: usize,
    pub labels_created: usize,
    pub tasks_created: usize,
    pub images_created: usize,
    pub annotations_created: usize,
}

#[derive(Deserialize)]
pub struct CleanupImportRequest {
    pub s3_key: String,
}

#[derive(Serialize)]
pub struct CleanupImportResponse {
    pub status: String,
}

const IMPORT_COLORS: [&str; 20] = [
    "#E6194B", "#3CB44B", "#FFE119", "#4363D8", "#F58231",
    "#911EB4", "#42D4F4", "#F032E6", "#BFEF45", "#FABED4",
    "#469990", "#DCBEFF", "#9A6324", "#FFFAC8", "#800000",
    "#AAFFC3", "#808000", "#FFD8B1", "#000075", "#A9A9A9",
];

fn normalize_key(value: &str) -> String {
    value.trim().to_lowercase()
}
fn count_map_to_attr_map(
    counts: &HashMap<String, u32>,
) -> HashMap<String, aws_sdk_dynamodb::types::AttributeValue> {
    counts
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                aws_sdk_dynamodb::types::AttributeValue::N(v.to_string()),
            )
        })
        .collect()
}

fn resolve_cvat_label_name(raw_name: &str) -> String {
    match normalize_key(raw_name).as_str() {
        "walls_gf" => "gf-wall".to_string(),
        "windows_gf" => "gf-window".to_string(),
        "roof_gf" => "gf-roof".to_string(),
        "walls_ff" => "ff-wall".to_string(),
        "windows_ff" => "ff-window".to_string(),
        "roof_ff" => "ff-roof".to_string(),
        "walls_2f" => "2f-wall".to_string(),
        "windows_2f" => "2f-window".to_string(),
        "roof_2f" => "2f-roof".to_string(),
        "walls_3f" => "3f-wall".to_string(),
        "windows_3f" => "3f-window".to_string(),
        "roof_3f" => "3f-roof".to_string(),
        "walls_sf" => "sf-wall".to_string(),
        "windows_sf" => "sf-window".to_string(),
        "roof_sf" => "sf-roof".to_string(),
        "walls_basement" => "basement-walls".to_string(),
        "windows_basement" => "basement-windows".to_string(),
        "window_screening" => "window-screening".to_string(),
        _ => raw_name.trim().to_string(),
    }
}

fn build_task_key(task_name: &str, subset: Option<&str>) -> String {
    match subset.map(str::trim).filter(|s| !s.is_empty()) {
        Some(subset) => format!("{}::{}", task_name.trim(), subset),
        None => task_name.trim().to_string(),
    }
}

fn format_import_task_name(task_name: &str, subset: Option<&str>) -> String {
    match subset.map(str::trim).filter(|s| !s.is_empty()) {
        Some(subset) => format!("{} [{}]", task_name.trim(), subset),
        None => task_name.trim().to_string(),
    }
}

fn child_text(node: roxmltree::Node<'_, '_>, child_name: &str) -> Option<String> {
    node.children()
        .find(|c| c.is_element() && c.has_tag_name(child_name))
        .and_then(|c| c.text())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn parse_f64_attr(node: roxmltree::Node<'_, '_>, attr: &str) -> Option<f64> {
    node.attribute(attr)?.trim().parse::<f64>().ok()
}

fn ensure_task_spec(
    tasks: &mut Vec<XmlTaskSpec>,
    task_seen: &mut HashSet<String>,
    task_name: String,
    subset: Option<String>,
) -> String {
    let key = build_task_key(&task_name, subset.as_deref());
    if task_seen.insert(key.clone()) {
        tasks.push(XmlTaskSpec {
            key: key.clone(),
            task_name,
            subset,
        });
    }
    key
}

fn parse_xml_polygon(node: roxmltree::Node<'_, '_>) -> Option<XmlImageAnnotation> {
    use doxle_atoms::drawing::model::{Geometry, Point};

    let raw_label = node.attribute("label")?.trim();
    if raw_label.is_empty() {
        return None;
    }

    let points_attr = node.attribute("points")?.trim();
    if points_attr.is_empty() {
        return None;
    }

    let points: Vec<Point> = points_attr
        .split(';')
        .filter_map(|pair| {
            let mut coords = pair.split(',');
            let x = coords.next()?.trim().parse::<f64>().ok()?;
            let y = coords.next()?.trim().parse::<f64>().ok()?;
            Some(Point { x, y })
        })
        .collect();

    if points.len() < 3 {
        return None;
    }

    Some(XmlImageAnnotation {
        label_name: resolve_cvat_label_name(raw_label),
        geometry: Geometry::Polygon { points },
    })
}

fn parse_xml_box(node: roxmltree::Node<'_, '_>) -> Option<XmlImageAnnotation> {
    use doxle_atoms::drawing::model::{Geometry, Point};

    let raw_label = node.attribute("label")?.trim();
    if raw_label.is_empty() {
        return None;
    }

    let xtl = parse_f64_attr(node, "xtl")?;
    let ytl = parse_f64_attr(node, "ytl")?;
    let xbr = parse_f64_attr(node, "xbr")?;
    let ybr = parse_f64_attr(node, "ybr")?;

    Some(XmlImageAnnotation {
        label_name: resolve_cvat_label_name(raw_label),
        geometry: Geometry::BBox {
            start: Point { x: xtl, y: ytl },
            end: Point { x: xbr, y: ybr },
        },
    })
}

fn parse_cvat_xml_dataset(
    xml_bytes: &[u8],
    import_id: &str,
    xml_file_name: Option<&str>,
) -> Result<XmlImportDataset, String> {
    let lossy = String::from_utf8_lossy(xml_bytes);
    let xml_text = lossy.trim_start_matches('\u{feff}');

    let doc = roxmltree::Document::parse(xml_text)
        .map_err(|e| format!("Failed to parse XML: {}", e))?;

    let task_dir_hint = xml_file_name
        .and_then(|name| name.split('/').next())
        .filter(|segment| segment.starts_with("task_"))
        .map(str::to_string);
    let xml_parent_dir = xml_file_name
        .and_then(|name| name.rsplit_once('/').map(|(parent, _)| parent.to_string()));

    let default_task_name = task_dir_hint
        .clone()
        .unwrap_or_else(|| format!("Import {}", &import_id[..8.min(import_id.len())]));

    let mut labels: Vec<String> = Vec::new();
    let mut labels_seen: HashSet<String> = HashSet::new();

    for label_node in doc
        .descendants()
        .filter(|n| n.is_element() && n.has_tag_name("label"))
    {
        if let Some(name) = child_text(label_node, "name") {
            let resolved = resolve_cvat_label_name(&name);
            if !resolved.is_empty() {
                let key = normalize_key(&resolved);
                if labels_seen.insert(key) {
                    labels.push(resolved);
                }
            }
        }
    }

    let mut tasks: Vec<XmlTaskSpec> = Vec::new();
    let mut task_seen: HashSet<String> = HashSet::new();
    let mut task_id_to_key: HashMap<String, String> = HashMap::new();

    for task_node in doc
        .descendants()
        .filter(|n| n.is_element() && n.has_tag_name("task"))
    {
        let Some(task_name) = child_text(task_node, "name") else { continue };
        let subset = child_text(task_node, "subset");
        let key = ensure_task_spec(&mut tasks, &mut task_seen, task_name, subset);

        if let Some(task_id) = child_text(task_node, "id") {
            task_id_to_key.insert(task_id, key);
        }
    }

    let mut images: Vec<XmlImageEntry> = Vec::new();

    for image_node in doc
        .descendants()
        .filter(|n| n.is_element() && n.has_tag_name("image"))
    {
        let Some(raw_file_name) = image_node
            .attribute("name")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
        else {
            continue;
        };
        let file_name = if raw_file_name.contains('/') {
            raw_file_name
        } else if let Some(parent) = &xml_parent_dir {
            format!("{}/{}", parent, raw_file_name)
        } else {
            raw_file_name
        };

        let subset_attr = image_node
            .attribute("subset")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        let mut task_key = image_node
            .attribute("task_id")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .and_then(|task_id| task_id_to_key.get(task_id).cloned());

        if task_key.is_none() {
            if let Some(task_name_attr) = image_node
                .attribute("task_name")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
            {
                task_key = Some(ensure_task_spec(
                    &mut tasks,
                    &mut task_seen,
                    task_name_attr,
                    subset_attr.clone(),
                ));
            }
        }

        if task_key.is_none() {
            if let Some(subset) = subset_attr.clone() {
                let base_name = if tasks.len() == 1 {
                    tasks[0].task_name.clone()
                } else {
                    default_task_name.clone()
                };
                task_key = Some(ensure_task_spec(
                    &mut tasks,
                    &mut task_seen,
                    base_name,
                    Some(subset),
                ));
            }
        }

        if task_key.is_none() && tasks.len() == 1 {
            task_key = Some(tasks[0].key.clone());
        }

        if task_key.is_none() {
            if let Some(prefix) = file_name
                .split('/')
                .next()
                .filter(|_| file_name.contains('/'))
            {
                if let Some(existing) = tasks
                    .iter()
                    .find(|t| t.task_name.eq_ignore_ascii_case(prefix))
                {
                    task_key = Some(existing.key.clone());
                }
            }
        }

        let task_key = task_key.unwrap_or_else(|| {
            ensure_task_spec(
                &mut tasks,
                &mut task_seen,
                default_task_name.clone(),
                None,
            )
        });

        let mut annotations: Vec<XmlImageAnnotation> = Vec::new();
        for shape_node in image_node.children().filter(|n| n.is_element()) {
            let parsed = match shape_node.tag_name().name() {
                "polygon" => parse_xml_polygon(shape_node),
                "box" => parse_xml_box(shape_node),
                _ => None,
            };
            if let Some(annotation) = parsed {
                let label_key = normalize_key(&annotation.label_name);
                if labels_seen.insert(label_key) {
                    labels.push(annotation.label_name.clone());
                }
                annotations.push(annotation);
            }
        }

        images.push(XmlImageEntry {
            file_name,
            task_key,
            annotations,
            source_s3_key: None,
            source_width: None,
            source_height: None,
            source_order: None,
        });
    }

    if tasks.is_empty() {
        ensure_task_spec(&mut tasks, &mut task_seen, default_task_name, None);
    }

    Ok(XmlImportDataset {
        labels,
        tasks,
        images,
    })
}

async fn process_cvat_xml_dataset(
    s3_client: &S3Client,
    dynamo_client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
    bucket: &str,
    files: &HashMap<String, Vec<u8>>,
    dataset: XmlImportDataset,
) -> Result<ProcessImportResponse, Error> {
    let existing_labels = super::labels::fetch_labels_for_block(dynamo_client, table_name, block_id).await?;
    let mut label_lookup: HashMap<String, (String, String)> = existing_labels
        .into_iter()
        .map(|l| {
            (
                normalize_key(&l.label_name),
                (l.label_id.clone(), l.label_name.clone()),
            )
        })
        .collect();

    let mut labels_created = 0usize;
    for raw_label_name in &dataset.labels {
        let resolved_name = resolve_cvat_label_name(raw_label_name);
        if resolved_name.is_empty() {
            continue;
        }

        let label_key = normalize_key(&resolved_name);
        if label_lookup.contains_key(&label_key) {
            continue;
        }

        let color = IMPORT_COLORS[labels_created % IMPORT_COLORS.len()];
        let label_payload = serde_json::json!({
            "label_name": resolved_name,
            "label_color": color
        });
        let label_body = serde_json::to_vec(&label_payload)?;
        let label_resp = super::labels::create_label(dynamo_client, table_name, block_id, &label_body).await?;

        let label_json: serde_json::Value = serde_json::from_slice(label_resp.body().as_ref())?;
        let label_id = label_json["label_id"].as_str().unwrap_or_default().to_string();
        if !label_id.is_empty() {
            label_lookup.insert(label_key, (label_id, resolved_name));
            labels_created += 1;
        }
    }

    let mut task_key_to_task_id: HashMap<String, String> = HashMap::new();
    let mut tasks_created = 0usize;

    for task_spec in &dataset.tasks {
        let task_name = format_import_task_name(&task_spec.task_name, task_spec.subset.as_deref());
        let task_payload = doxle_atoms::tasks::model::CreateTaskPayload {
            task_name,
            assignee: None,
            checked_by: None,
        };
        let task = doxle_atoms::tasks::service::create_task(dynamo_client, table_name, block_id, task_payload)
            .await
            .map_err(|e| format!("Failed to create task: {}", e))?;
        task_key_to_task_id.insert(task_spec.key.clone(), task.task_id);
        tasks_created += 1;
    }

    if task_key_to_task_id.is_empty() {
        let task_payload = doxle_atoms::tasks::model::CreateTaskPayload {
            task_name: "Import".to_string(),
            assignee: None,
            checked_by: None,
        };
        let task = doxle_atoms::tasks::service::create_task(dynamo_client, table_name, block_id, task_payload)
            .await
            .map_err(|e| format!("Failed to create fallback task: {}", e))?;
        task_key_to_task_id.insert("Import".to_string(), task.task_id);
        tasks_created = 1;
    }

    let mut order_per_task: HashMap<String, i32> = HashMap::new();
    let mut images_created = 0usize;
    let mut annotations_created = 0usize;

    for image in &dataset.images {
        let Some(task_id) = task_key_to_task_id
            .get(&image.task_key)
            .cloned()
            .or_else(|| task_key_to_task_id.values().next().cloned())
        else {
            continue;
        };

        let Some(bytes) = find_image_in_files(files, &image.file_name) else {
            tracing::warn!("⚠️ XML import image not found in zip: {}", image.file_name);
            continue;
        };

        let image_id = uuid::Uuid::new_v4().to_string();
        let ext = image
            .file_name
            .rsplit('.')
            .next()
            .unwrap_or("png")
            .to_lowercase();
        let s3_image_key = format!("annotations/blocks/{}/images/{}.{}", block_id, image_id, ext);

        s3_client
            .put_object()
            .bucket(bucket)
            .key(&s3_image_key)
            .body(bytes.clone().into())
            .content_type(format!("image/{}", ext))
            .send()
            .await
            .map_err(|e| format!("Failed to upload image {}: {}", image.file_name, e))?;

        let order = order_per_task.entry(task_id.clone()).or_insert(0);
        // Try to get image dimensions from bytes
        let dims = image::load_from_memory(bytes).ok().map(|img| (img.width(), img.height()));

        doxle_atoms::media::service::create_image_for_task(
            dynamo_client,
            table_name,
            project_id,
            block_id,
            &task_id,
            image_id.clone(),
            image.file_name.rsplit('/').next().unwrap_or(&image.file_name).to_string(),
            s3_image_key.clone(),
            Some(*order),
            dims.map(|(w, _)| w),
            dims.map(|(_, h)| h),
        )
        .await
        .map_err(|e| format!("Failed to create image record: {}", e))?;
        *order += 1;
        images_created += 1;

        for annotation in &image.annotations {
            let label_key = normalize_key(&annotation.label_name);
            let Some((label_id, label_name)) = label_lookup.get(&label_key).cloned() else {
                continue;
            };

            let payload = doxle_atoms::drawing::model::CreateAnnotationPayload {
                annotation_id: uuid::Uuid::new_v4().to_string(),
                label_id,
                label_name,
                geometry: annotation.geometry.clone(),
            };

            doxle_atoms::drawing::service::create_annotation(
                dynamo_client,
                table_name,
                Some(project_id),
                block_id,
                &image_id,
                "IMPORT",
                payload,
            )
            .await
            .map_err(|e| format!("Failed to create annotation: {}", e))?;

            annotations_created += 1;
        }
    }

    Ok(ProcessImportResponse {
        status: "complete".to_string(),
        labels_created,
        tasks_created,
        images_created,
        annotations_created,
    })
}

/// Process an uploaded CVAT/COCO zip: parse JSON, create task, upload images, create annotations.
/// Uses existing service functions so DynamoDB structure is identical to manual creation.
pub async fn process_import(
    s3_client: &S3Client,
    dynamo_client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let request: ProcessImportRequest = serde_json::from_slice(body)?;
    let bucket = get_bucket_name();

    tracing::info!("📦 Starting import processing: block={}, key={}", block_id, request.s3_key);

    // ── Step 1: Download zip from S3 into memory ──
    let get_result = s3_client
        .get_object()
        .bucket(&bucket)
        .key(&request.s3_key)
        .send()
        .await
        .map_err(|e| format!("Failed to download zip from S3: {}", e))?;

    let zip_bytes = get_result
        .body
        .collect()
        .await
        .map_err(|e| format!("Failed to read zip body: {}", e))?
        .into_bytes();

    tracing::info!("📦 Downloaded zip: {} bytes", zip_bytes.len());

    let files = extract_all_files(zip_bytes.as_ref())?;

    // ── Step 2A: Prefer CVAT XML import (supports task/subset structure) ──
    let xml_names = find_all_cvat_xml(&files);
    if !xml_names.is_empty() {
        tracing::info!("📦 Found {} XML import manifest(s)", xml_names.len());

        let mut datasets = Vec::new();
        for xml_name in &xml_names {
            let xml_bytes = files.get(xml_name)
                .ok_or_else(|| format!("XML file {} not found", xml_name))?;
            let dataset = parse_cvat_xml_dataset(xml_bytes, &request.import_id, Some(xml_name))?;
            datasets.push(dataset);
        }
        let dataset = merge_xml_import_datasets(datasets);
        tracing::info!(
            "📦 Parsed XML: {} tasks, {} images, {} labels",
            dataset.tasks.len(),
            dataset.images.len(),
            dataset.labels.len()
        );

        let response = process_cvat_xml_dataset(
            s3_client,
            dynamo_client,
            table_name,
            project_id,
            block_id,
            &bucket,
            &files,
            dataset,
        )
        .await?;

        let _ = s3_client
            .delete_object()
            .bucket(&bucket)
            .key(&request.s3_key)
            .send()
            .await;

        tracing::info!("📦 XML import complete for block {}", block_id);

        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(serde_json::to_string(&response)?.into())
            .map_err(Box::new)?);
    }

    // ── Step 2: Find and parse COCO JSON ──
    let coco_json_name = find_coco_json(&files)
        .ok_or("No COCO JSON found in zip (expected annotations/*.json)")?;

    let coco: CocoDataset = {
        let json_bytes = files.get(&coco_json_name)
            .ok_or_else(|| format!("JSON file {} not found", coco_json_name))?;
        serde_json::from_slice(json_bytes)
            .map_err(|e| format!("Failed to parse COCO JSON: {}", e))?
    };

    tracing::info!("📦 Parsed COCO: {} images, {} annotations, {} categories",
        coco.images.len(), coco.annotations.len(), coco.categories.len());

    // ── Step 3: Match/create labels ──
    // CVAT → Doxle name aliases (CVAT uses different naming convention)
    let cvat_to_doxle: HashMap<&str, &str> = HashMap::from([
        ("walls_gf", "gf-wall"),
        ("windows_gf", "gf-window"),
        ("roof_gf", "gf-roof"),
        ("walls_ff", "ff-wall"),
        ("windows_ff", "ff-window"),
        ("roof_ff", "ff-roof"),
        ("walls_sf", "sf-wall"),
        ("windows_sf", "sf-window"),
        ("roof_sf", "sf-roof"),
        ("walls_basement", "basement-walls"),
        ("windows_basement", "basement-windows"),
    ]);

    let existing_labels = super::labels::fetch_labels_for_block(dynamo_client, table_name, block_id).await?;
    let mut cat_id_to_label: HashMap<u64, (String, String)> = HashMap::new(); // coco_cat_id -> (label_id, label_name)
    let mut labels_created: usize = 0;

    // Generate import colors for new labels
    let import_colors = [
        "#E6194B", "#3CB44B", "#FFE119", "#4363D8", "#F58231",
        "#911EB4", "#42D4F4", "#F032E6", "#BFEF45", "#FABED4",
        "#469990", "#DCBEFF", "#9A6324", "#FFFAC8", "#800000",
        "#AAFFC3", "#808000", "#FFD8B1", "#000075", "#A9A9A9",
    ];

    for cat in &coco.categories {
        // Resolve CVAT name to Doxle name via alias map, fallback to original
        let resolved_name = cvat_to_doxle
            .get(cat.name.to_lowercase().as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| cat.name.clone());

        // Try to match by resolved name (case-insensitive)
        if let Some(existing) = existing_labels.iter().find(|l| l.label_name.to_lowercase() == resolved_name.to_lowercase()) {
            cat_id_to_label.insert(cat.id, (existing.label_id.clone(), existing.label_name.clone()));
        } else {
            // Create new label using resolved name
            let color = import_colors[labels_created % import_colors.len()];
            let label_payload = serde_json::json!({
                "label_name": resolved_name,
                "label_color": color
            });
            let label_body = serde_json::to_vec(&label_payload)?;
            let label_resp = super::labels::create_label(dynamo_client, table_name, block_id, &label_body).await?;

            // Parse label_id from response
            let label_json: serde_json::Value = serde_json::from_slice(
                label_resp.body().as_ref()
            )?;
            let label_id = label_json["label_id"].as_str().unwrap_or_default().to_string();
            cat_id_to_label.insert(cat.id, (label_id, resolved_name));
            labels_created += 1;
        }
    }

    tracing::info!("📦 Labels: {} existing matched, {} created", cat_id_to_label.len() - labels_created, labels_created);

    // ── Step 4: Create task ──
    let task_name = format!("Import {}", &request.import_id[..8.min(request.import_id.len())]);
    let task_payload = doxle_atoms::tasks::model::CreateTaskPayload {
        task_name,
        assignee: None,
        checked_by: None,
    };
    let task = doxle_atoms::tasks::service::create_task(dynamo_client, table_name, block_id, task_payload)
        .await
        .map_err(|e| format!("Failed to create task: {}", e))?;

    tracing::info!("📦 Created task: {}", task.task_id);

    // ── Step 5: Upload images to S3 + create DynamoDB records ──
    let mut coco_img_id_to_image_id: HashMap<u64, String> = HashMap::new();
    let mut images_created: usize = 0;

    for (order, coco_img) in coco.images.iter().enumerate() {
        let image_id = uuid::Uuid::new_v4().to_string();
        let ext = coco_img.file_name.rsplit('.').next().unwrap_or("png");
        let s3_image_key = format!("annotations/blocks/{}/images/{}.{}", block_id, image_id, ext);

        // Find image file in zip (could be under images/ or default/ etc)
        let image_bytes = find_image_in_files(&files, &coco_img.file_name);

        if let Some(bytes) = image_bytes {
            // Upload to S3
            s3_client
                .put_object()
                .bucket(&bucket)
                .key(&s3_image_key)
                .body(bytes.clone().into())
                .content_type(format!("image/{}", ext))
                .send()
                .await
                .map_err(|e| format!("Failed to upload image {}: {}", coco_img.file_name, e))?;

            // Create DynamoDB image record using existing service
            let image_url = s3_image_key.clone();
            doxle_atoms::media::service::create_image_for_task(
                dynamo_client,
                table_name,
                project_id,
                block_id,
                &task.task_id,
                image_id.clone(),
                coco_img.file_name.rsplit('/').next().unwrap_or(&coco_img.file_name).to_string(),
                image_url,
                Some(order as i32),
                Some(coco_img.width),
                Some(coco_img.height),
            )
            .await
            .map_err(|e| format!("Failed to create image record: {}", e))?;

            coco_img_id_to_image_id.insert(coco_img.id, image_id);
            images_created += 1;

            if images_created % 100 == 0 {
                tracing::info!("📦 Images uploaded: {}/{}", images_created, coco.images.len());
            }
        } else {
            tracing::warn!("⚠️ Image not found in zip: {}", coco_img.file_name);
        }
    }

    tracing::info!("📦 All images uploaded: {}", images_created);

    // ── Step 6: Create annotations ──
    let mut annotations_created: usize = 0;

    for coco_ann in &coco.annotations {
        let image_id = match coco_img_id_to_image_id.get(&coco_ann.image_id) {
            Some(id) => id,
            None => continue, // skip if image wasn't found
        };
        let (label_id, label_name) = match cat_id_to_label.get(&coco_ann.category_id) {
            Some(pair) => pair.clone(),
            None => continue, // skip if category wasn't mapped
        };

        // Convert COCO geometry to our format
        let geometry = coco_to_geometry(coco_ann);
        let Some(geometry) = geometry else { continue };

        let payload = doxle_atoms::drawing::model::CreateAnnotationPayload {
            annotation_id: uuid::Uuid::new_v4().to_string(),
            label_id,
            label_name,
            geometry,
        };

        doxle_atoms::drawing::service::create_annotation(
            dynamo_client,
            table_name,
            Some(project_id),
            block_id,
            image_id,
            "IMPORT",
            payload,
        )
        .await
        .map_err(|e| format!("Failed to create annotation: {}", e))?;

        annotations_created += 1;

        if annotations_created % 500 == 0 {
            tracing::info!("📦 Annotations created: {}/{}", annotations_created, coco.annotations.len());
        }
    }

    tracing::info!("📦 All annotations created: {}", annotations_created);

    // ── Step 7: Clean up — delete the zip from imports/ ──
    let _ = s3_client
        .delete_object()
        .bucket(&bucket)
        .key(&request.s3_key)
        .send()
        .await;

    tracing::info!("📦 Import complete for block {}", block_id);

    let response = ProcessImportResponse {
        status: "complete".to_string(),
        labels_created,
        tasks_created: 1,
        images_created,
        annotations_created,
    };

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(serde_json::to_string(&response)?.into())
        .map_err(Box::new)?)
}

/// Extract all files from a zip archive into memory (sync, so no Send issues)
fn extract_all_files(zip_bytes: &[u8]) -> Result<HashMap<String, Vec<u8>>, String> {
    let mut archive = ZipArchive::new(Cursor::new(zip_bytes))
        .map_err(|e| format!("Failed to open zip: {}", e))?;
    let mut files = HashMap::new();
    for i in 0..archive.len() {
        let mut f = archive.by_index(i).map_err(|e| format!("Zip error: {}", e))?;
        if f.is_dir() { continue; }
        let name = f.name().to_string();
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).map_err(|e| format!("Read error: {}", e))?;
        files.insert(name, buf);
    }
    Ok(files)
}

/// Check if file content looks like XML (starts with `<` after optional BOM/whitespace)
fn looks_like_xml(bytes: &[u8]) -> bool {
    let s = bytes.iter().position(|&b| !b.is_ascii_whitespace() && b != 0xEF && b != 0xBB && b != 0xBF);
    matches!(s, Some(i) if bytes[i] == b'<')
}


/// Find all CVAT XML manifests in stable order
fn find_all_cvat_xml(files: &HashMap<String, Vec<u8>>) -> Vec<String> {
    let mut xml_names: Vec<String> = files
        .iter()
        .filter(|(n, bytes)| n.ends_with(".xml") && looks_like_xml(bytes))
        .map(|(n, _)| n.clone())
        .collect();

    // Prefer annotation-like files first when sorting
    xml_names.sort_by_key(|name| {
        let preferred = name.contains("annotations") || name.contains("cvat");
        (!preferred, name.clone())
    });
    xml_names
}

fn find_coco_json(files: &HashMap<String, Vec<u8>>) -> Option<String> {
    if let Some(name) = files.keys().find(|n| n.ends_with(".json") && (n.contains("instances") || n.contains("annotations"))) {
        return Some(name.clone());
    }
    files.keys().find(|n| n.ends_with(".json")).cloned()
}

/// Find an image by filename (exact match or basename match)
fn find_image_in_files<'a>(files: &'a HashMap<String, Vec<u8>>, file_name: &str) -> Option<&'a Vec<u8>> {
    if let Some(bytes) = files.get(file_name) {
        return Some(bytes);
    }
    let target = file_name.rsplit('/').next().unwrap_or(file_name);
    let mut matches = files
        .iter()
        .filter(|(name, _)| name.rsplit('/').next().unwrap_or(name) == target);

    let first = matches.next();
    let second = matches.next();

    match (first, second) {
        (Some((_, bytes)), None) => Some(bytes),
        (Some(_), Some(_)) => {
            tracing::warn!(
                "⚠️ Ambiguous basename match for '{}'; multiple files found in zip. Skipping to avoid wrong annotation mapping.",
                file_name
            );
            None
        }
        _ => None,
    }
}

/// Convert a COCO annotation to our Geometry type
fn coco_to_geometry(ann: &CocoAnnotation) -> Option<doxle_atoms::drawing::model::Geometry> {
    use doxle_atoms::drawing::model::{Geometry, Point};

    // Prefer polygon segmentation if available
    if let Some(segs) = ann.segmentation.as_array() {
        if let Some(first_seg) = segs.first() {
            if let Some(coords) = first_seg.as_array() {
                let points: Vec<Point> = coords
                    .chunks(2)
                    .filter_map(|pair| {
                        if pair.len() == 2 {
                            Some(Point {
                                x: pair[0].as_f64().unwrap_or(0.0),
                                y: pair[1].as_f64().unwrap_or(0.0),
                            })
                        } else {
                            None
                        }
                    })
                    .collect();
                if points.len() >= 3 {
                    return Some(Geometry::Polygon { points });
                }
            }
        }
    }

    // Fallback to bbox [x, y, w, h]
    if ann.bbox.len() == 4 {
        return Some(Geometry::BBox {
            start: Point { x: ann.bbox[0], y: ann.bbox[1] },
            end: Point { x: ann.bbox[0] + ann.bbox[2], y: ann.bbox[1] + ann.bbox[3] },
        });
    }

    None
}

/// Convert a COCO dataset into the unified XmlImportDataset format
fn coco_to_import_dataset(coco: &CocoDataset, import_id: &str) -> XmlImportDataset {
    let labels: Vec<String> = coco.categories.iter().map(|c| resolve_cvat_label_name(&c.name)).collect();

    let task_name = format!("Import {}", &import_id[..8.min(import_id.len())]);
    let tasks = vec![XmlTaskSpec {
        key: task_name.clone(),
        task_name: task_name.clone(),
        subset: None,
    }];

    // Build coco_image_id → index map
    let img_id_to_idx: HashMap<u64, usize> = coco.images.iter().enumerate().map(|(i, img)| (img.id, i)).collect();

    // Build coco_category_id → resolved label name
    let cat_id_to_name: HashMap<u64, String> = coco.categories.iter().map(|c| (c.id, resolve_cvat_label_name(&c.name))).collect();

    // Pre-create image entries
    let mut images: Vec<XmlImageEntry> = coco.images.iter().map(|img| XmlImageEntry {
        file_name: img.file_name.clone(),
        task_key: task_name.clone(),
        annotations: Vec::new(),
        source_s3_key: None,
        source_width: Some(img.width),
        source_height: Some(img.height),
        source_order: None,
    }).collect();

    // Flatten annotations into their images
    for ann in &coco.annotations {
        let Some(&img_idx) = img_id_to_idx.get(&ann.image_id) else { continue };
        let Some(label_name) = cat_id_to_name.get(&ann.category_id) else { continue };
        let Some(geometry) = coco_to_geometry(ann) else { continue };
        images[img_idx].annotations.push(XmlImageAnnotation {
            label_name: label_name.clone(),
            geometry,
        });
    }

    XmlImportDataset { labels, tasks, images }
}

#[derive(Debug, Clone, Deserialize)]
struct DoxleExportBundle {
    #[serde(default)]
    format: String,
    #[serde(default)]
    labels: Vec<DoxleExportLabel>,
    #[serde(default)]
    tasks: Vec<DoxleExportTask>,
    #[serde(default)]
    images: Vec<DoxleExportImage>,
}

#[derive(Debug, Clone, Deserialize)]
struct DoxleExportLabel {
    #[serde(default)]
    label_name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct DoxleExportTask {
    #[serde(default)]
    task_id: String,
    #[serde(default)]
    task_name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct DoxleExportAnnotation {
    #[serde(default)]
    label_name: String,
    geometry: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
struct DoxleExportImage {
    #[serde(default)]
    image_name: String,
    #[serde(default)]
    s3_key: String,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    order: Option<i32>,
    #[serde(default)]
    annotations: Vec<DoxleExportAnnotation>,
}

fn find_doxle_export_json(files: &HashMap<String, Vec<u8>>) -> Option<String> {
    files
        .keys()
        .find(|name| name.ends_with("doxle_export.json"))
        .cloned()
}

fn parse_backup_geometry(value: &serde_json::Value) -> Option<doxle_atoms::drawing::model::Geometry> {
    match value {
        serde_json::Value::String(s) => serde_json::from_str::<doxle_atoms::drawing::model::Geometry>(s).ok(),
        other => serde_json::from_value::<doxle_atoms::drawing::model::Geometry>(other.clone()).ok(),
    }
}

fn doxle_export_to_import_dataset(
    bundle: DoxleExportBundle,
    import_id: &str,
) -> Result<XmlImportDataset, String> {
    let mut labels: Vec<String> = Vec::new();
    let mut label_seen: HashSet<String> = HashSet::new();

    for label in &bundle.labels {
        let resolved = resolve_cvat_label_name(&label.label_name);
        if resolved.is_empty() {
            continue;
        }
        let key = normalize_key(&resolved);
        if label_seen.insert(key) {
            labels.push(resolved);
        }
    }

    let mut tasks: Vec<XmlTaskSpec> = Vec::new();
    let mut task_seen: HashSet<String> = HashSet::new();
    let mut task_id_to_key: HashMap<String, String> = HashMap::new();

    for (idx, task) in bundle.tasks.iter().enumerate() {
        let task_name = task.task_name.trim();
        if task_name.is_empty() {
            continue;
        }
        let key = if !task.task_id.trim().is_empty() {
            task.task_id.clone()
        } else {
            format!("task-{}", idx)
        };
        if task_seen.insert(key.clone()) {
            tasks.push(XmlTaskSpec {
                key: key.clone(),
                task_name: task_name.to_string(),
                subset: None,
            });
        }
        if !task.task_id.trim().is_empty() {
            task_id_to_key.insert(task.task_id.clone(), key);
        }
    }

    if tasks.is_empty() {
        let fallback = format!("Import {}", &import_id[..8.min(import_id.len())]);
        tasks.push(XmlTaskSpec {
            key: fallback.clone(),
            task_name: fallback,
            subset: None,
        });
    }
    let default_task_key = tasks[0].key.clone();

    let mut images: Vec<XmlImageEntry> = Vec::new();
    for (idx, image) in bundle.images.iter().enumerate() {
        let task_key = image
            .task_id
            .as_ref()
            .and_then(|task_id| task_id_to_key.get(task_id))
            .cloned()
            .unwrap_or_else(|| default_task_key.clone());

        let mut annotations: Vec<XmlImageAnnotation> = Vec::new();
        for ann in &image.annotations {
            let resolved_label = resolve_cvat_label_name(&ann.label_name);
            if resolved_label.is_empty() {
                continue;
            }
            let Some(geometry) = parse_backup_geometry(&ann.geometry) else {
                continue;
            };
            let key = normalize_key(&resolved_label);
            if label_seen.insert(key) {
                labels.push(resolved_label.clone());
            }
            annotations.push(XmlImageAnnotation {
                label_name: resolved_label,
                geometry,
            });
        }

        images.push(XmlImageEntry {
            file_name: if image.image_name.trim().is_empty() {
                format!("backup-image-{}", idx)
            } else {
                image.image_name.clone()
            },
            task_key,
            annotations,
            source_s3_key: if image.s3_key.trim().is_empty() {
                None
            } else {
                Some(image.s3_key.clone())
            },
            source_width: image.width,
            source_height: image.height,
            source_order: image.order,
        });
    }

    Ok(XmlImportDataset {
        labels,
        tasks,
        images,
    })
}

/// Re-zip all files with an added _manifest.json
fn rezip_with_manifest(files: &HashMap<String, Vec<u8>>, manifest: &ImportManifest) -> Result<Vec<u8>, String> {
    let buf = Vec::new();
    let mut writer = ZipWriter::new(Cursor::new(buf));
    let options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for (name, bytes) in files {
        if name == "_manifest.json" { continue; } // skip old manifest if present
        writer.start_file(name, options).map_err(|e| format!("Zip write error: {}", e))?;
        writer.write_all(bytes).map_err(|e| format!("Zip write error: {}", e))?;
    }

    let manifest_bytes = serde_json::to_vec_pretty(manifest).map_err(|e| format!("JSON error: {}", e))?;
    writer.start_file("_manifest.json", options).map_err(|e| format!("Zip write error: {}", e))?;
    writer.write_all(&manifest_bytes).map_err(|e| format!("Zip write error: {}", e))?;

    let cursor = writer.finish().map_err(|e| format!("Zip finish error: {}", e))?;
    Ok(cursor.into_inner())
}

// ─── STEP 1: Parse Import ───

pub async fn parse_import(
    s3_client: &S3Client,
    block_id: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let request: ParseImportRequest = serde_json::from_slice(body)?;
    let bucket = get_bucket_name();

    tracing::info!("📦 Parse import: block={}, key={}", block_id, request.s3_key);

    // Download zip from S3
    let get_result = s3_client
        .get_object()
        .bucket(&bucket)
        .key(&request.s3_key)
        .send()
        .await
        .map_err(|e| format!("Failed to download zip: {}", e))?;

    let zip_bytes = get_result
        .body
        .collect()
        .await
        .map_err(|e| format!("Failed to read zip: {}", e))?
        .into_bytes();

    let files = extract_all_files(zip_bytes.as_ref())?;

    tracing::info!("📦 Zip contains {} files: {:?}", files.len(),
        files.keys().map(|k| format!("{} ({}B)", k, files[k].len())).collect::<Vec<_>>());

    // Detect format and parse
    let xml_names = find_all_cvat_xml(&files);
    let (format_str, dataset) = if let Some(backup_json_name) = find_doxle_export_json(&files) {
        let backup_bytes = files
            .get(&backup_json_name)
            .ok_or("Doxle export JSON file not found")?;
        let bundle: DoxleExportBundle = serde_json::from_slice(backup_bytes)
            .map_err(|e| format!("Failed to parse Doxle export JSON: {}", e))?;
        if !bundle.format.starts_with("doxle_export_") {
            return Err(format!("Unsupported Doxle export format: {}", bundle.format).into());
        }
        let ds = doxle_export_to_import_dataset(bundle, &request.import_id)?;
        ("doxle_export".to_string(), ds)
    } else if !xml_names.is_empty() {
        let mut datasets = Vec::new();
        for xml_name in &xml_names {
            let xml_bytes = files.get(xml_name).ok_or("XML file not found")?;
            let dataset = parse_cvat_xml_dataset(xml_bytes, &request.import_id, Some(xml_name))?;
            datasets.push(dataset);
        }
        ("cvat_xml".to_string(), merge_xml_import_datasets(datasets))
    } else if let Some(json_name) = find_coco_json(&files) {
        let json_bytes = files.get(&json_name).ok_or("JSON file not found")?;
        let coco: CocoDataset = serde_json::from_slice(json_bytes)
            .map_err(|e| format!("Failed to parse COCO JSON: {}", e))?;
        let ds = coco_to_import_dataset(&coco, &request.import_id);
        ("coco_json".to_string(), ds)
    } else {
        return Ok(Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "error": "No supported import format found in zip (expected doxle_export.json, CVAT XML, or COCO JSON)"
                })
                .to_string()
                .into(),
            )
            .map_err(Box::new)?);
    };

    // If max_tasks is specified, truncate tasks and filter images to only those tasks
    let dataset = if let Some(max) = request.max_tasks {
        if max < dataset.tasks.len() {
            let allowed_keys: std::collections::HashSet<String> = dataset.tasks[..max]
                .iter()
                .map(|t| t.key.clone())
                .collect();
            let filtered_tasks = dataset.tasks[..max].to_vec();
            let filtered_images: Vec<XmlImageEntry> = dataset.images
                .into_iter()
                .filter(|img| allowed_keys.contains(&img.task_key))
                .collect();
            tracing::info!("📦 max_tasks={}: kept {} tasks, {} images (from original)",
                max, filtered_tasks.len(), filtered_images.len());
            XmlImportDataset {
                labels: dataset.labels,
                tasks: filtered_tasks,
                images: filtered_images,
            }
        } else {
            dataset
        }
    } else {
        dataset
    };

    let total_annotations: usize = dataset.images.iter().map(|img| img.annotations.len()).sum();

    let manifest = ImportManifest {
        format: format_str.clone(),
        import_id: request.import_id.clone(),
        dataset,
        total_labels: 0, // set below
        total_tasks: 0,
        total_images: 0,
        total_annotations: total_annotations,
    };
    let manifest = ImportManifest {
        total_labels: manifest.dataset.labels.len(),
        total_tasks: manifest.dataset.tasks.len(),
        total_images: manifest.dataset.images.len(),
        ..manifest
    };

    tracing::info!("📦 Parsed: {} labels, {} tasks, {} images, {} annotations",
        manifest.total_labels, manifest.total_tasks, manifest.total_images, manifest.total_annotations);

    // Re-zip with manifest
    let new_zip = rezip_with_manifest(&files, &manifest)?;

    // Re-upload to S3
    s3_client
        .put_object()
        .bucket(&bucket)
        .key(&request.s3_key)
        .body(new_zip.into())
        .content_type("application/zip")
        .send()
        .await
        .map_err(|e| format!("Failed to re-upload zip: {}", e))?;

    let response = ParseImportResponse {
        format: format_str,
        total_labels: manifest.total_labels,
        total_tasks: manifest.total_tasks,
        total_images: manifest.total_images,
        total_annotations: manifest.total_annotations,
    };

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(serde_json::to_string(&response)?.into())
        .map_err(Box::new)?)
}

// ─── STEP 2: Process Batch ───

pub async fn process_batch(
    s3_client: &S3Client,
    dynamo_client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let request: ProcessBatchRequest = serde_json::from_slice(body)?;
    let bucket = get_bucket_name();

    tracing::info!("📦 Process batch: phase={}, offset={}, limit={}", request.phase, request.offset, request.limit);

    // Download zip and read manifest
    let get_result = s3_client
        .get_object()
        .bucket(&bucket)
        .key(&request.s3_key)
        .send()
        .await
        .map_err(|e| format!("Failed to download zip: {}", e))?;

    let zip_bytes = get_result
        .body
        .collect()
        .await
        .map_err(|e| format!("Failed to read zip: {}", e))?
        .into_bytes();

    let files = extract_all_files(zip_bytes.as_ref())?;

    let manifest_bytes = files.get("_manifest.json")
        .ok_or("_manifest.json not found in zip")?;
    let manifest: ImportManifest = serde_json::from_slice(manifest_bytes)
        .map_err(|e| format!("Failed to parse manifest: {}", e))?;

    let dataset = &manifest.dataset;
    let offset = request.offset;
    let limit = request.limit;

    let mut labels_created = 0usize;
    let mut tasks_created = 0usize;
    let mut images_created = 0usize;
    let mut annotations_created = 0usize;
    let mut processed = 0usize;
    let total;

    match request.phase.as_str() {
        "labels" => {
            total = dataset.labels.len();
            let slice_end = (offset + limit).min(total);

            // Fetch existing labels to avoid duplicates
            let mut existing_labels = super::labels::fetch_labels_for_block(dynamo_client, table_name, block_id).await?;

            // For doxle export restores, prune stale zero-count labels that are not in the incoming import.
            if offset == 0 && manifest.format == "doxle_export" {
                let import_labels: HashSet<String> = dataset
                    .labels
                    .iter()
                    .map(|name| normalize_key(&resolve_cvat_label_name(name)))
                    .filter(|name| !name.is_empty())
                    .collect();

                let mut pruned = 0usize;
                for label in &existing_labels {
                    let key = normalize_key(&label.label_name);
                    let is_unused = !import_labels.contains(&key);
                    let is_empty = label.label_count == 0 && label.bbox_count == 0 && label.polygon_count == 0;
                    if is_unused && is_empty {
                        dynamo_client
                            .delete_item()
                            .table_name(table_name)
                            .key("PK", aws_sdk_dynamodb::types::AttributeValue::S(format!("BLOCK#{}", block_id)))
                            .key(
                                "SK",
                                aws_sdk_dynamodb::types::AttributeValue::S(format!("LABEL#{}", label.label_id)),
                            )
                            .send()
                            .await
                            .map_err(|e| format!("Failed to prune stale label {}: {}", label.label_name, e))?;
                        pruned += 1;
                    }
                }

                if pruned > 0 {
                    tracing::info!("📦 Pruned {} stale zero-count labels before doxle export import", pruned);
                    existing_labels = super::labels::fetch_labels_for_block(dynamo_client, table_name, block_id).await?;
                }
            }

            let mut existing_set: HashSet<String> = existing_labels.iter().map(|l| normalize_key(&l.label_name)).collect();

            for raw_label_name in &dataset.labels[offset..slice_end] {
                let resolved = resolve_cvat_label_name(raw_label_name);
                if resolved.is_empty() { processed += 1; continue; }

                let key = normalize_key(&resolved);
                if existing_set.contains(&key) { processed += 1; continue; }

                let color = IMPORT_COLORS[(offset + processed) % IMPORT_COLORS.len()];
                let label_payload = serde_json::json!({
                    "label_name": resolved,
                    "label_color": color
                });
                let label_body = serde_json::to_vec(&label_payload)?;
                let _ = super::labels::create_label(dynamo_client, table_name, block_id, &label_body).await?;
                existing_set.insert(key);
                labels_created += 1;
                processed += 1;
            }
        }
        "tasks" => {
            total = dataset.tasks.len();
            let slice_end = (offset + limit).min(total);

            for task_spec in &dataset.tasks[offset..slice_end] {
                let task_name = format_import_task_name(&task_spec.task_name, task_spec.subset.as_deref());
                let task_payload = doxle_atoms::tasks::model::CreateTaskPayload {
                    task_name,
                    assignee: None,
                    checked_by: None,
                };
                doxle_atoms::tasks::service::create_task(dynamo_client, table_name, block_id, task_payload)
                    .await
                    .map_err(|e| format!("Failed to create task: {}", e))?;
                tasks_created += 1;
                processed += 1;
            }
        }
        "images" => {
            total = dataset.images.len();
            let slice_end = (offset + limit).min(total);

            // Build label_name → label_id lookup from existing labels
            let existing_labels = super::labels::fetch_labels_for_block(dynamo_client, table_name, block_id).await?;
            let label_lookup: HashMap<String, (String, String)> = existing_labels
                .into_iter()
                .map(|l| (normalize_key(&l.label_name), (l.label_id.clone(), l.label_name.clone())))
                .collect();

            // Build task_name → task_id lookup from existing tasks
            let existing_tasks = doxle_atoms::tasks::service::load_tasks_for_block(dynamo_client, table_name, block_id)
                .await
                .map_err(|e| format!("Failed to load tasks: {}", e))?;
            let task_name_to_id: HashMap<String, String> = existing_tasks
                .into_iter()
                .map(|t| (t.task_name.clone(), t.task_id.clone()))
                .collect();

            // Compute per-task order starting values by counting images before this offset
            let mut order_per_task: HashMap<String, i32> = HashMap::new();
            for img in &dataset.images[..offset.min(total)] {
                let task_display_name = dataset.tasks.iter()
                    .find(|t| t.key == img.task_key)
                    .map(|t| format_import_task_name(&t.task_name, t.subset.as_deref()))
                    .unwrap_or_else(|| img.task_key.clone());
                *order_per_task.entry(task_display_name).or_insert(0) += 1;
            }

            for image in &dataset.images[offset..slice_end] {
                // Resolve task_key → task_id
                let task_display_name = dataset.tasks.iter()
                    .find(|t| t.key == image.task_key)
                    .map(|t| format_import_task_name(&t.task_name, t.subset.as_deref()))
                    .unwrap_or_else(|| image.task_key.clone());

                let Some(task_id) = task_name_to_id.get(&task_display_name)
                    .or_else(|| task_name_to_id.values().next())
                else {
                    processed += 1;
                    continue;
                };


                let image_id = uuid::Uuid::new_v4().to_string();
                let mut width = image.source_width;
                let mut height = image.source_height;
                let s3_image_key = if let Some(source_key) = image
                    .source_s3_key
                    .as_ref()
                    .filter(|k| !k.trim().is_empty())
                {
                    source_key.clone()
                } else {
                    let Some(bytes) = find_image_in_files(&files, &image.file_name) else {
                        tracing::warn!("⚠️ Image not found in zip: {}", image.file_name);
                        processed += 1;
                        continue;
                    };
                    let ext = image.file_name.rsplit('.').next().unwrap_or("png").to_lowercase();
                    let generated_key = format!("annotations/blocks/{}/images/{}.{}", block_id, image_id, ext);

                    // Upload image to S3 when image bytes are present in zip
                    s3_client
                        .put_object()
                        .bucket(&bucket)
                        .key(&generated_key)
                        .body(bytes.clone().into())
                        .content_type(format!("image/{}", ext))
                        .send()
                        .await
                        .map_err(|e| format!("Failed to upload image {}: {}", image.file_name, e))?;

                    if width.is_none() || height.is_none() {
                        if let Some((w, h)) = image::load_from_memory(bytes).ok().map(|img| (img.width(), img.height())) {
                            if width.is_none() {
                                width = Some(w);
                            }
                            if height.is_none() {
                                height = Some(h);
                            }
                        }
                    }

                    generated_key
                };

                // Create DynamoDB image record
                let order = order_per_task.entry(task_display_name.clone()).or_insert(0);
                let assigned_order = image.source_order.unwrap_or(*order);
                doxle_atoms::media::service::create_image_for_task(
                    dynamo_client,
                    table_name,
                    project_id,
                    block_id,
                    task_id,
                    image_id.clone(),
                    image.file_name.rsplit('/').next().unwrap_or(&image.file_name).to_string(),
                    s3_image_key.clone(),
                    Some(assigned_order),
                    width,
                    height,
                )
                .await
                .map_err(|e| format!("Failed to create image record: {}", e))?;
                *order = (*order).max(assigned_order + 1);
                images_created += 1;

                // Batch write annotations for this image (25 per BatchWriteItem)
                let mut ann_write_reqs: Vec<aws_sdk_dynamodb::types::WriteRequest> = Vec::new();
                let mut image_labels_count: HashMap<String, u32> = HashMap::new();
                let mut image_bbox_count: HashMap<String, u32> = HashMap::new();
                let mut image_polygon_count: HashMap<String, u32> = HashMap::new();
                let mut image_annotation_count = 0usize;
                let now = chrono::Utc::now().to_rfc3339();
                for annotation in &image.annotations {
                    let label_key = normalize_key(&annotation.label_name);
                    let Some((label_id, label_name)) = label_lookup.get(&label_key).cloned() else { continue };
                    let map_label_name = label_name.clone();

                    let ann_id = uuid::Uuid::new_v4().to_string();
                    let geometry_json = serde_json::to_string(&annotation.geometry)
                        .unwrap_or_default();

                    let mut item = HashMap::new();
                    item.insert("PK".to_string(), aws_sdk_dynamodb::types::AttributeValue::S(format!("IMAGE#{}", image_id)));
                    item.insert("SK".to_string(), aws_sdk_dynamodb::types::AttributeValue::S(format!("ANNOTATION#{}", ann_id)));
                    item.insert("label_id".to_string(), aws_sdk_dynamodb::types::AttributeValue::S(label_id));
                    item.insert("label_name".to_string(), aws_sdk_dynamodb::types::AttributeValue::S(label_name));
                    item.insert("geometry".to_string(), aws_sdk_dynamodb::types::AttributeValue::S(geometry_json));
                    item.insert("created_by".to_string(), aws_sdk_dynamodb::types::AttributeValue::S("IMPORT".to_string()));
                    item.insert("created_at".to_string(), aws_sdk_dynamodb::types::AttributeValue::S(now.clone()));

                    ann_write_reqs.push(
                        aws_sdk_dynamodb::types::WriteRequest::builder()
                            .put_request(
                                aws_sdk_dynamodb::types::PutRequest::builder()
                                    .set_item(Some(item))
                                    .build()
                                    .unwrap(),
                            )
                            .build(),
                    );
                    *image_labels_count.entry(map_label_name.clone()).or_insert(0) += 1;
                    match &annotation.geometry {
                        doxle_atoms::drawing::model::Geometry::BBox { .. } => {
                            *image_bbox_count.entry(map_label_name).or_insert(0) += 1;
                        }
                        doxle_atoms::drawing::model::Geometry::Polygon { .. } => {
                            *image_polygon_count.entry(map_label_name).or_insert(0) += 1;
                        }
                    }
                    image_annotation_count += 1;
                    annotations_created += 1;
                }

                // Write in batches of 25
                for chunk in ann_write_reqs.chunks(25) {
                    dynamo_client
                        .batch_write_item()
                        .request_items(table_name, chunk.to_vec())
                        .send()
                        .await
                        .map_err(|e| format!("Failed batch writing annotations: {}", e))?;
                }
                // Persist per-image aggregate counts so block/task label counters stay correct.
                dynamo_client
                    .update_item()
                    .table_name(table_name)
                    .key("PK", aws_sdk_dynamodb::types::AttributeValue::S(format!("BLOCK#{}", block_id)))
                    .key("SK", aws_sdk_dynamodb::types::AttributeValue::S(format!("IMAGE#{}", image_id)))
                    .update_expression(
                        "SET annotation_count = :annotation_count, labels_count = :labels_count, bbox_count = :bbox_count, polygon_count = :polygon_count",
                    )
                    .expression_attribute_values(
                        ":annotation_count",
                        aws_sdk_dynamodb::types::AttributeValue::N(image_annotation_count.to_string()),
                    )
                    .expression_attribute_values(
                        ":labels_count",
                        aws_sdk_dynamodb::types::AttributeValue::M(count_map_to_attr_map(&image_labels_count)),
                    )
                    .expression_attribute_values(
                        ":bbox_count",
                        aws_sdk_dynamodb::types::AttributeValue::M(count_map_to_attr_map(&image_bbox_count)),
                    )
                    .expression_attribute_values(
                        ":polygon_count",
                        aws_sdk_dynamodb::types::AttributeValue::M(count_map_to_attr_map(&image_polygon_count)),
                    )
                    .send()
                    .await
                    .map_err(|e| format!("Failed updating image counters after import batch: {}", e))?;

                processed += 1;
            }

            if annotations_created > 0 {
                dynamo_client
                    .update_item()
                    .table_name(table_name)
                    .key(
                        "PK",
                        aws_sdk_dynamodb::types::AttributeValue::S(format!("PROJECT#{}", project_id)),
                    )
                    .key(
                        "SK",
                        aws_sdk_dynamodb::types::AttributeValue::S(format!("BLOCK#{}", block_id)),
                    )
                    .update_expression(
                        "SET annotation_count = if_not_exists(annotation_count, :zero) + :delta, block_updated_at = :updated_at",
                    )
                    .expression_attribute_values(
                        ":zero",
                        aws_sdk_dynamodb::types::AttributeValue::N("0".to_string()),
                    )
                    .expression_attribute_values(
                        ":delta",
                        aws_sdk_dynamodb::types::AttributeValue::N(annotations_created.to_string()),
                    )
                    .expression_attribute_values(
                        ":updated_at",
                        aws_sdk_dynamodb::types::AttributeValue::S(chrono::Utc::now().to_rfc3339()),
                    )
                    .send()
                    .await
                    .map_err(|e| format!("Failed updating block annotation counters after import batch: {}", e))?;
            }
        }
        _ => {
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header("Content-Type", "application/json")
                .body(serde_json::json!({"error": format!("Unknown phase: {}", request.phase)}).to_string().into())
                .map_err(Box::new)?);
        }
    }

    tracing::info!("📦 Batch done: phase={}, processed={}, labels={}, tasks={}, images={}, annotations={}",
        request.phase, processed, labels_created, tasks_created, images_created, annotations_created);

    let response = ProcessBatchResponse {
        phase: request.phase,
        processed,
        total,
        labels_created,
        tasks_created,
        images_created,
        annotations_created,
    };

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(serde_json::to_string(&response)?.into())
        .map_err(Box::new)?)
}

// ─── STEP 3: Cleanup ───

pub async fn cleanup_import(
    s3_client: &S3Client,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let request: CleanupImportRequest = serde_json::from_slice(body)?;
    let bucket = get_bucket_name();

    let _ = s3_client
        .delete_object()
        .bucket(&bucket)
        .key(&request.s3_key)
        .send()
        .await;

    tracing::info!("📦 Cleanup: deleted {}", request.s3_key);

    let response = CleanupImportResponse {
        status: "cleaned".to_string(),
    };

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(serde_json::to_string(&response)?.into())
        .map_err(Box::new)?)
}

/// Complete multipart upload for import zip
pub async fn complete_import_upload(
    s3_client: &S3Client,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let request: CompleteImportUploadRequest = serde_json::from_slice(body)?;

    let mut completed_parts = Vec::new();
    for part in &request.parts {
        let completed_part = aws_sdk_s3::types::CompletedPart::builder()
            .part_number(part.part_number)
            .e_tag(&part.etag)
            .build();
        completed_parts.push(completed_part);
    }

    let completed_upload = aws_sdk_s3::types::CompletedMultipartUpload::builder()
        .set_parts(Some(completed_parts))
        .build();

    s3_client
        .complete_multipart_upload()
        .bucket(&get_bucket_name())
        .key(&request.s3_key)
        .upload_id(&request.upload_id)
        .multipart_upload(completed_upload)
        .send()
        .await
        .map_err(|e| format!("Failed to complete multipart upload: {}", e))?;

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(
            serde_json::json!({
                "import_id": request.import_id,
                "s3_key": request.s3_key,
                "status": "uploaded"
            })
            .to_string()
            .into(),
        )
        .map_err(Box::new)?)
}

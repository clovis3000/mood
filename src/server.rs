use crate::fetch;
use crate::store::{AddOpts, Item, Place, Store, Target, TIERS};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{header, Method, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

const INDEX: &str = include_str!("../assets/index.html");

struct AppErr(anyhow::Error);
impl IntoResponse for AppErr {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, format!("{:#}", self.0)).into_response()
    }
}
impl<E: Into<anyhow::Error>> From<E> for AppErr {
    fn from(e: E) -> Self {
        AppErr(e.into())
    }
}
type R<T> = Result<T, AppErr>;

pub async fn serve(store: Store, port: u16, open: bool) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/", get(index))
        .route("/b/{board}", get(index))
        .route("/api/boards", get(boards).post(create_board))
        .route("/api/current", post(set_current))
        .route("/api/b/{board}", get(board))
        .route("/api/b/{board}/rev", get(rev))
        .route("/api/b/{board}/file/{file}", get(file))
        .route("/api/b/{board}/img/{size}/{file}", get(sized))
        .route("/api/b/{board}/items", post(upload))
        .route("/api/b/{board}/fetch", post(fetch_url))
        .route("/api/b/{board}/items/{id}", patch(patch_item).delete(delete_item))
        .route("/api/b/{board}/restore/{id}", post(restore))
        .route("/api/b/{board}/items/{id}/trim", post(trim))
        .route("/api/b/{board}/items/{id}/rate", post(rate))
        .route("/api/b/{board}/notes/{target}", post(add_note))
        .route("/api/b/{board}/notes/{target}/{nid}", delete(del_note))
        .layer(DefaultBodyLimit::max(200 << 20))
        .layer(middleware::from_fn(move |req, next| guard(port, req, next)))
        .with_state(store.clone());

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let url = format!("http://localhost:{port}/b/{}", store.current()?);
    eprintln!("mood · {} · {url}", store.root.display());
    if open {
        std::process::Command::new("open").arg(&url).spawn().ok();
    }
    axum::serve(listener, app).await?;
    Ok(())
}

/// Header every write must carry. A browser won't let another site add a
/// custom header without a CORS preflight, and this server never approves
/// one, so a page you happen to visit can't write to your boards.
const WRITE_HEADER: &str = "x-mood";

async fn guard(port: u16, req: Request, next: Next) -> Response {
    // DNS rebinding: a site can point its own name at 127.0.0.1 and become
    // same-origin, custom headers and all, but its requests still carry its Host.
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    if host != format!("localhost:{port}") && host != format!("127.0.0.1:{port}") {
        return (StatusCode::FORBIDDEN, "mood only answers to localhost").into_response();
    }
    let read = matches!(*req.method(), Method::GET | Method::HEAD);
    if !read && !req.headers().contains_key(WRITE_HEADER) {
        return (StatusCode::FORBIDDEN, "writes need an x-mood header").into_response();
    }
    next.run(req).await
}

async fn index() -> Html<String> {
    // MOOD_DEV=1 serves the page from source so edits show on reload.
    if std::env::var_os("MOOD_DEV").is_some() {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/index.html");
        if let Ok(s) = std::fs::read_to_string(p) {
            return Html(s);
        }
    }
    Html(INDEX.to_string())
}

#[derive(Deserialize)]
struct BoardsQ {
    /// include a few preview images per board (the home page); the board menu doesn't need them
    #[serde(default)]
    preview: bool,
}

/// Images a home-page preview can show: the best-rated first, then the newest.
const PREVIEW_TILES: usize = 6;

async fn boards(State(s): State<Store>, Query(q): Query<BoardsQ>) -> R<Json<Value>> {
    let mut v = vec![];
    for name in s.boards()? {
        let b = s.board(&name)?;
        let items = b.items()?;
        let mut o = json!({"name": name, "count": items.len()});
        if q.preview {
            let mut best: Vec<_> = items.iter().collect();
            best.sort_by_key(|it| (std::cmp::Reverse(it.rating.unwrap_or(0)), std::cmp::Reverse(it.id)));
            o["modified"] = json!(b.modified());
            o["items"] = best
                .iter()
                .take(PREVIEW_TILES)
                .map(|it| json!({"id": it.id, "file": it.file, "added": it.added}))
                .collect();
        }
        v.push(o);
    }
    Ok(Json(json!({"boards": v, "current": s.current()?})))
}

#[derive(Deserialize)]
struct NameBody {
    name: String,
}

async fn create_board(State(s): State<Store>, Json(b): Json<NameBody>) -> R<Json<Value>> {
    s.create(&b.name)?;
    s.set_current(&b.name)?;
    Ok(Json(json!({"name": b.name})))
}

async fn set_current(State(s): State<Store>, Json(b): Json<NameBody>) -> R<Json<Value>> {
    s.set_current(&b.name)?;
    Ok(Json(json!({"ok": true})))
}

async fn board(State(s): State<Store>, Path(name): Path<String>) -> R<Json<Value>> {
    let b = s.board(&name)?;
    Ok(Json(json!({
        "name": b.name,
        "meta": b.meta()?,
        "items": b.items()?,
        "rev": b.rev(),
        "dir": b.dir,
    })))
}

async fn rev(State(s): State<Store>, Path(name): Path<String>) -> R<Json<Value>> {
    Ok(Json(json!({"rev": s.board(&name)?.rev()})))
}

async fn file(State(s): State<Store>, Path((name, file)): Path<(String, String)>) -> R<Response> {
    serve_file(&s.board(&name)?.file_path(&file)?).await
}

/// Decoding a 2000px png holds ~16MB while it resizes; a fresh board asks for
/// every image at once, so cap how many are in flight.
static RESIZING: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

/// A copy at most `size` px on its long side (see `Board::tier`), or the original when that's no bigger.
async fn sized(State(s): State<Store>, Path((name, size, file)): Path<(String, u32, String)>) -> R<Response> {
    let b = s.board(&name)?;
    let original = b.file_path(&file)?;
    let _permit = RESIZING.acquire().await?;
    let p = tokio::task::spawn_blocking(move || b.tier(&file, size)).await??;
    serve_file(&p.unwrap_or(original)).await
}

async fn serve_file(p: &std::path::Path) -> R<Response> {
    let bytes: Vec<u8> = tokio::fs::read(p).await?;
    let ct = match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "png" => "image/png",
        "jpg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    };
    Ok((
        [(header::CONTENT_TYPE, ct), (header::CACHE_CONTROL, "max-age=31536000, immutable")],
        bytes,
    )
        .into_response())
}

/// Make a new image's sized copies in the background, so the first zoom doesn't wait on a resize.
fn warm(s: &Store, board: &str, it: &Item) {
    let (Ok(b), file) = (s.board(board), it.file.clone()) else { return };
    tokio::spawn(async move {
        let Ok(_permit) = RESIZING.acquire().await else { return };
        tokio::task::spawn_blocking(move || b.tier(&file, TIERS[0])).await.ok();
    });
}

#[derive(Deserialize)]
struct UploadQ {
    cx: Option<f64>,
    cy: Option<f64>,
    #[serde(default)]
    source: String,
    #[serde(default)]
    title: String,
    by: Option<String>,
    iw: Option<u32>,
    ih: Option<u32>,
}

fn place(cx: Option<f64>, cy: Option<f64>) -> Place {
    match (cx, cy) {
        (Some(x), Some(y)) => Place::Center(x, y),
        _ => Place::Beside,
    }
}

async fn upload(
    State(s): State<Store>,
    Path(name): Path<String>,
    Query(q): Query<UploadQ>,
    body: Bytes,
) -> R<Json<Value>> {
    let b = s.board(&name)?;
    let it = b.add_image(
        &body,
        AddOpts {
            place: place(q.cx, q.cy),
            w: None,
            title: q.title,
            source: q.source,
            by: q.by.unwrap_or_else(|| "lewis".into()),
            hint: q.iw.zip(q.ih),
        },
    )?;
    warm(&s, &name, &it);
    Ok(Json(json!(it)))
}

#[derive(Deserialize)]
struct FetchBody {
    url: String,
    cx: Option<f64>,
    cy: Option<f64>,
    by: Option<String>,
}

async fn fetch_url(State(s): State<Store>, Path(name): Path<String>, Json(f): Json<FetchBody>) -> R<Json<Value>> {
    let b = s.board(&name)?;
    let url = f.url.clone();
    let got = tokio::task::spawn_blocking(move || fetch::fetch(&url)).await??;
    let it = b.add_image(
        &got.bytes,
        AddOpts {
            place: place(f.cx, f.cy),
            w: None,
            title: got.title,
            source: got.source,
            by: f.by.unwrap_or_else(|| "lewis".into()),
            hint: None,
        },
    )?;
    warm(&s, &name, &it);
    Ok(Json(json!(it)))
}

#[derive(Deserialize)]
struct PatchBody {
    x: Option<f64>,
    y: Option<f64>,
    w: Option<f64>,
    z: Option<i64>,
    title: Option<String>,
    source: Option<String>,
}

async fn patch_item(
    State(s): State<Store>,
    Path((name, id)): Path<(String, u32)>,
    Json(p): Json<PatchBody>,
) -> R<Json<Value>> {
    let b = s.board(&name)?;
    let mut it = b.item(id)?;
    if let Some(v) = p.x { it.x = v.round(); }
    if let Some(v) = p.y { it.y = v.round(); }
    if let Some(v) = p.w { it.w = v.max(20.0).round(); }
    if let Some(v) = p.z { it.z = v; }
    if let Some(v) = p.title { it.title = v.trim().into(); }
    if let Some(v) = p.source { it.source = v.trim().into(); }
    b.save_item(&it)?;
    Ok(Json(json!(it)))
}

async fn delete_item(State(s): State<Store>, Path((name, id)): Path<(String, u32)>) -> R<Json<Value>> {
    s.board(&name)?.remove(id)?;
    Ok(Json(json!({"ok": true})))
}

async fn restore(State(s): State<Store>, Path((name, id)): Path<(String, u32)>) -> R<Json<Value>> {
    Ok(Json(json!(s.board(&name)?.restore(id)?)))
}

#[derive(Deserialize)]
struct NoteBody {
    text: String,
    kind: Option<String>,
    by: Option<String>,
}

async fn add_note(
    State(s): State<Store>,
    Path((name, target)): Path<(String, String)>,
    Json(n): Json<NoteBody>,
) -> R<Json<Value>> {
    let t: Target = target.parse()?;
    let note = s.board(&name)?.add_note(
        t,
        n.by.as_deref().unwrap_or("lewis"),
        n.kind.as_deref().unwrap_or("note"),
        &n.text,
    )?;
    Ok(Json(json!(note)))
}

async fn del_note(
    State(s): State<Store>,
    Path((name, target, nid)): Path<(String, String, String)>,
) -> R<Json<Value>> {
    s.board(&name)?.remove_note(target.parse()?, &nid)?;
    Ok(Json(json!({"ok": true})))
}

async fn trim(State(s): State<Store>, Path((name, id)): Path<(String, u32)>) -> R<Json<Value>> {
    match s.board(&name)?.trim(id)? {
        Some(it) => Ok(Json(json!(it))),
        None => Err(anyhow::anyhow!("no uniform border to trim").into()),
    }
}

#[derive(Deserialize)]
struct RateBody {
    /// 1–9, or null/absent to clear
    rating: Option<u8>,
}

async fn rate(
    State(s): State<Store>,
    Path((name, id)): Path<(String, u32)>,
    Json(r): Json<RateBody>,
) -> R<Json<Value>> {
    Ok(Json(json!(s.board(&name)?.rate(id, r.rating)?)))
}

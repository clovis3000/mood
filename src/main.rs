mod fetch;
mod render;
mod server;
mod store;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use store::{parse_cell, parse_note_text, AddOpts, Board, Item, Note, Place, Store, Target, CELL};

/// Moodboards on disk. A browser canvas for the human, this CLI for the AI,
/// and plain files underneath that both of them (and Finder) can read.
///
/// Board layout: $MOOD_HOME (default ~/moodboards)/<board>/items/<id>.{jpg,png,..}
/// with a <id>.json sidecar holding position, title, source and notes.
/// The canvas is divided into 1000-unit cells named like a chessboard
/// (A1 top-left of the origin, columns A..Z, rows 1..). The browser shows the
/// same grid, so cells and #ids are a shared vocabulary.
///
/// Notes: `by` is "lewis" or "claude". Kinds: like (+), dislike (−), note (·).
#[derive(Parser)]
#[command(name = "mood", version, verbatim_doc_comment)]
struct Cli {
    /// Board to act on (default: $MOOD_BOARD, else the one open in the browser)
    #[arg(short, long, global = true)]
    board: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the canvas at http://localhost:<port>
    Serve {
        #[arg(short, long, default_value_t = 7777)]
        port: u16,
        /// Open the browser too
        #[arg(long)]
        open: bool,
    },
    /// List boards
    Boards,
    /// Create a board and make it current
    New { name: String },
    /// Make a board current
    Use { name: String },
    /// List items: id, cell, rating, size, title/source, note tallies
    Ls {
        #[arg(long)]
        json: bool,
        /// Sort by Lewis's rating, highest first
        #[arg(long)]
        by_rating: bool,
    },
    /// Rate an item 1–9 (1 repels · 5 indifferent · 9 essential); 0 clears
    Rate { id: String, rating: u8 },
    /// Show one item (or `board`) in full, with the image's absolute path
    Show {
        target: String,
        #[arg(long)]
        json: bool,
    },
    /// Every item and every note as markdown: read this to synthesise a style
    Dump,
    /// Annotate an item (or `board`). Prefix text with + or - for like/dislike.
    Note {
        target: String,
        /// The note. "+ the grain" = like, "- too glossy" = dislike
        #[arg(required = true, num_args = 1.., allow_hyphen_values = true)]
        text: Vec<String>,
        #[arg(long, conflicts_with = "dislike")]
        like: bool,
        #[arg(long)]
        dislike: bool,
        /// Author (default: claude — this CLI is the AI's side of the board)
        #[arg(long, default_value = "claude")]
        by: String,
    },
    /// Delete a note by its id (shown in `show`)
    Unnote { target: String, note_id: String },
    /// Add an image from a file path or URL (a page URL uses its og:image)
    Add {
        src: String,
        #[arg(long, default_value = "")]
        source: String,
        #[arg(long)]
        title: Option<String>,
        /// Drop it in the first free spot in this cell, e.g. C3 (default: beside the newest item's group)
        #[arg(long)]
        at: Option<String>,
        /// World width (default: even-area sizing)
        #[arg(long)]
        w: Option<f64>,
        #[arg(long, default_value = "claude")]
        by: String,
        /// Crop uniform borders after adding
        #[arg(long)]
        trim: bool,
    },
    /// Edit an item's title/source/position/width
    Set {
        id: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        source: Option<String>,
        /// Move into the first free spot in this cell
        #[arg(long)]
        at: Option<String>,
        #[arg(long)]
        w: Option<f64>,
    },
    /// Crop uniform borders (scan padding) off items; originals go to .originals/
    Trim {
        #[arg(required = true)]
        ids: Vec<String>,
    },
    /// Move an item to the trash (restorable with `restore`)
    Rm { id: String },
    /// Bring an item back from the trash
    Restore { id: String },
    /// Render the board to PNG and print its path. REGION: (none) whole board,
    /// C3 one cell, B2:D4 a range, #12 around an item.
    View {
        region: Option<String>,
        /// Longest side in pixels
        #[arg(long, default_value_t = 1600)]
        px: u32,
        /// Output path (default: <board>/.views/<region>.png)
        #[arg(short, long)]
        out: Option<std::path::PathBuf>,
    },
    /// Print the board's directory
    Path,
}

fn pick_board(s: &Store, flag: &Option<String>) -> Result<Board> {
    let name = match flag.clone().or_else(|| std::env::var("MOOD_BOARD").ok()) {
        Some(n) => n,
        None => s.current()?,
    };
    s.board(&name)
}

fn id_of(s: &str) -> Result<u32> {
    s.trim_start_matches('#').parse().with_context(|| format!("not an item id: {s}"))
}

fn tally(notes: &[Note]) -> String {
    let c = |k: &str| notes.iter().filter(|n| n.kind == k).count();
    let by = |who: &str| notes.iter().filter(|n| (n.by == "claude") == (who == "claude")).count();
    if notes.is_empty() {
        return String::new();
    }
    format!("+{} −{} ·{}  [L{} C{}]", c("like"), c("dislike"), c("note"), by("lewis"), by("claude"))
}

fn glyph(kind: &str) -> &'static str {
    match kind {
        "like" => "+",
        "dislike" => "−",
        _ => "·",
    }
}

fn short_date(at: &str) -> &str {
    at.get(..16).unwrap_or(at)
}

fn print_notes(notes: &[Note], indent: &str) {
    for n in notes {
        println!(
            "{indent}{:<9}{} {}   ({} · {})",
            format!("[{}]", n.by),
            glyph(&n.kind),
            n.text.replace('\n', &format!("\n{indent}           ")),
            n.id,
            short_date(&n.at).replace('T', " ")
        );
    }
}

fn cell_origin(spec: &str) -> Result<(f64, f64)> {
    let (c, r) = parse_cell(spec)?;
    Ok((c as f64 * CELL + 40.0, r as f64 * CELL + 60.0))
}

fn describe(it: &Item) -> String {
    let mut s = String::new();
    if !it.title.is_empty() {
        s += &format!("\"{}\"", it.title);
    }
    if !it.source.is_empty() {
        if !s.is_empty() {
            s += " — ";
        }
        s += &it.source;
    }
    if s.is_empty() {
        s = "(no title/source)".into();
    }
    s
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let store = Store::open()?;

    match cli.cmd {
        Cmd::Serve { port, open } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(server::serve(store, port, open))?;
        }
        Cmd::Boards => {
            let cur = store.current()?;
            for b in store.boards()? {
                let n = store.board(&b)?.items()?.len();
                println!("{} {b}  ({n})", if b == cur { "*" } else { " " });
            }
        }
        Cmd::New { name } => {
            store.create(&name)?;
            store.set_current(&name)?;
            println!("created {name} (now current)");
        }
        Cmd::Use { name } => {
            store.set_current(&name)?;
            println!("current: {name}");
        }
        Cmd::Ls { json, by_rating } => {
            let b = pick_board(&store, &cli.board)?;
            let items = b.items()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&items)?);
                return Ok(());
            }
            let m = b.meta()?;
            println!("{} · {} items · {} board notes · {}", b.name, items.len(), m.notes.len(), b.dir.display());
            let mut sorted = items.clone();
            sorted.sort_by_key(|i| i.id);
            if by_rating {
                sorted.sort_by_key(|i| std::cmp::Reverse(i.rating.unwrap_or(0)));
            }
            for it in sorted {
                println!(
                    "#{:<4} {:<4} {:<3} {:>4}×{:<4} {}  {}",
                    it.id,
                    it.cell(),
                    it.rating.map(|r| format!("{r}/9")).unwrap_or_else(|| "·".into()),
                    it.iw,
                    it.ih,
                    describe(&it),
                    tally(&it.notes)
                );
            }
        }
        Cmd::Show { target, json } => {
            let b = pick_board(&store, &cli.board)?;
            match target.parse::<Target>()? {
                Target::Board => {
                    let m = b.meta()?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&m)?);
                    } else {
                        println!("board {} · created {}", b.name, short_date(&m.created));
                        print_notes(&m.notes, "  ");
                    }
                }
                Target::Item(id) => {
                    let it = b.item(id)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&it)?);
                        return Ok(());
                    }
                    println!("#{} · {} · {}×{} px · w {} at ({}, {})", it.id, it.cell(), it.iw, it.ih, it.w, it.x, it.y);
                    println!("title:  {}", it.title);
                    println!("source: {}", it.source);
                    println!("rating: {}", store::rating_label(it.rating));
                    println!("added:  {} by {}", short_date(&it.added).replace('T', " "), it.added_by);
                    println!("file:   {}", b.items_dir().join(&it.file).display());
                    if it.notes.is_empty() {
                        println!("notes:  none");
                    } else {
                        println!("notes:");
                        print_notes(&it.notes, "  ");
                    }
                }
            }
        }
        Cmd::Dump => {
            let b = pick_board(&store, &cli.board)?;
            let m = b.meta()?;
            let mut items = b.items()?;
            items.sort_by_key(|i| i.id);
            println!("# {}\n", b.name);
            println!("{} items · dir {}\n", items.len(), b.dir.display());
            if !m.notes.is_empty() {
                println!("## Board notes\n");
                for n in &m.notes {
                    println!("- **{}** {} {}", n.by, glyph(&n.kind), n.text);
                }
                println!();
            }
            let mut rated: Vec<&Item> = items.iter().filter(|i| i.rating.is_some()).collect();
            if !rated.is_empty() {
                rated.sort_by_key(|i| std::cmp::Reverse(i.rating));
                println!("## Ratings (lewis, 1 repels · 5 indifferent · 9 essential)\n");
                for it in &rated {
                    println!("- {} #{} {}", store::rating_label(it.rating), it.id, describe(it));
                }
                let unrated = items.len() - rated.len();
                if unrated > 0 {
                    println!("- ({unrated} unrated)");
                }
                println!();
            }
            // group by cell so spatial clusters read as clusters
            let mut cells: Vec<String> = items.iter().map(|i| i.cell()).collect();
            cells.dedup();
            let mut seen = std::collections::BTreeSet::new();
            for cell in cells {
                if !seen.insert(cell.clone()) {
                    continue;
                }
                println!("## {cell}\n");
                for it in items.iter().filter(|i| i.cell() == cell) {
                    println!("### #{} {}\n", it.id, if it.title.is_empty() { "" } else { &it.title });
                    if !it.source.is_empty() {
                        println!("source: {}  ", it.source);
                    }
                    if it.rating.is_some() {
                        println!("rating: {}  ", store::rating_label(it.rating));
                    }
                    println!("file: {}\n", b.items_dir().join(&it.file).display());
                    for n in &it.notes {
                        println!("- **{}** {} {}", n.by, glyph(&n.kind), n.text);
                    }
                    println!();
                }
            }
        }
        Cmd::Note { target, text, like, dislike, by } => {
            let b = pick_board(&store, &cli.board)?;
            let (mut kind, body) = parse_note_text(&text.join(" "));
            if like {
                kind = "like".into();
            }
            if dislike {
                kind = "dislike".into();
            }
            let n = b.add_note(target.parse()?, &by, &kind, &body)?;
            println!("{} {} {}  ({})", by, glyph(&n.kind), n.text, n.id);
        }
        Cmd::Rate { id, rating } => {
            let b = pick_board(&store, &cli.board)?;
            let it = b.rate(id_of(&id)?, if rating == 0 { None } else { Some(rating) })?;
            println!("#{} {}", it.id, store::rating_label(it.rating));
        }
        Cmd::Unnote { target, note_id } => {
            let b = pick_board(&store, &cli.board)?;
            b.remove_note(target.parse()?, &note_id)?;
            println!("removed {note_id}");
        }
        Cmd::Add { src, source, title, at, w, by, trim } => {
            let b = pick_board(&store, &cli.board)?;
            let (bytes, source, title) = if src.starts_with("http://") || src.starts_with("https://") {
                let f = fetch::fetch(&src)?;
                let s = if source.is_empty() { f.source } else { source };
                (f.bytes, s, title.unwrap_or(f.title))
            } else {
                let p = std::path::Path::new(&src);
                let bytes = std::fs::read(p).with_context(|| format!("can't read {src}"))?;
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                (bytes, source, title.unwrap_or(stem))
            };
            let place = match at.as_deref() {
                Some(c) => {
                    let (x, y) = cell_origin(c)?;
                    Place::Near(x, y)
                }
                None => Place::Beside,
            };
            let it = b.add_image(&bytes, AddOpts { place, w, title, source, by, hint: None })?;
            let it = if trim { b.trim(it.id)?.unwrap_or(it) } else { it };
            println!("added #{} in {} · {}", it.id, it.cell(), describe(&it));
        }
        Cmd::Set { id, title, source, at, w } => {
            let b = pick_board(&store, &cli.board)?;
            let mut it = b.item(id_of(&id)?)?;
            if let Some(t) = title {
                it.title = t;
            }
            if let Some(s) = source {
                it.source = s;
            }
            if let Some(w) = w {
                it.w = w.max(20.0);
            }
            if let Some(c) = at {
                let (x0, y0) = cell_origin(&c)?;
                let others: Vec<Item> = b.items()?.into_iter().filter(|o| o.id != it.id).collect();
                let (x, y) = store::find_free(&others, it.w, it.h(), x0, y0);
                it.x = x;
                it.y = y;
            }
            b.save_item(&it)?;
            println!("#{} · {} · {}", it.id, it.cell(), describe(&it));
        }
        Cmd::Trim { ids } => {
            let b = pick_board(&store, &cli.board)?;
            for id in ids {
                let id = id_of(&id)?;
                match b.trim(id)? {
                    Some(it) => println!("#{id} trimmed to {}×{}", it.iw, it.ih),
                    None => println!("#{id} has no uniform border to trim"),
                }
            }
        }
        Cmd::Rm { id } => {
            let b = pick_board(&store, &cli.board)?;
            let id = id_of(&id)?;
            b.remove(id)?;
            println!("#{id} → trash (mood restore {id})");
        }
        Cmd::Restore { id } => {
            let b = pick_board(&store, &cli.board)?;
            let it = b.restore(id_of(&id)?)?;
            println!("restored #{} in {}", it.id, it.cell());
        }
        Cmd::View { region, px, out } => {
            let b = pick_board(&store, &cli.board)?;
            let items = b.items()?;
            let reg = render::parse_region(region.as_deref(), &items)?;
            let r = render::render(&b, &items, reg, px)?;
            let out = match out {
                Some(o) => o,
                None => {
                    let d = b.dir.join(".views");
                    std::fs::create_dir_all(&d)?;
                    let name = region
                        .as_deref()
                        .unwrap_or("all")
                        .replace([':', '#', '/'], "_");
                    d.join(format!("{name}.png"))
                }
            };
            r.img.save(&out)?;
            println!(
                "{} · {} · {}×{}px · {:.3} px/unit",
                b.name,
                r.region.label(),
                r.img.width(),
                r.img.height(),
                r.scale
            );
            println!("{}", out.display());
            for it in &r.visible {
                println!("  #{:<4} {:<4} {}  {}", it.id, it.cell(), describe(it), tally(&it.notes));
            }
            if r.visible.is_empty() {
                bail!("nothing in that region");
            }
        }
        Cmd::Path => {
            println!("{}", pick_board(&store, &cli.board)?.dir.display());
        }
    }
    Ok(())
}

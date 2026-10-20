# mood

Moodboards on disk. A browser canvas for you, a CLI for your AI, and plain files underneath that both of you can read.

A board is a folder of images, each with a small JSON sidecar holding its position, title, source, rating and notes. You arrange images on an infinite canvas in the browser. An AI agent such as Claude Code works on the same board from the terminal: it adds images, leaves notes, reads your ratings and renders the board to a PNG it can look at. The CLI only touches files, so it works with no server running, and the canvas picks up its changes within a couple of seconds.

The canvas is divided into a chess-style grid (A1, C3, …), and every image has a number (#12). The grid and the numbers look the same in the browser and in the CLI's renders, so "the warm cluster in C3" or "#12" means the same thing to both of you.

## Install

Needs a Rust toolchain.

```sh
cargo install --git https://github.com/clovis3000/mood
```

Developed on macOS. HEIC, TIFF and BMP images are converted with macOS's `sips`; everything else is portable.

## The canvas

```sh
mood serve --open        # http://localhost:7777
```

- Paste (⌘V) an image, a file or a page URL, or drop one in. It lands under the cursor.
- Scroll to pan. Pinch, ⌘-scroll, or scroll over the bar on the left edge to zoom.
- Drag images to arrange them; drag on empty space to select several.
- Press 1–9 to rate the selected image (1 repels, 5 indifferent, 9 essential).
- Press `?` for the rest of the keys.

Each image is loaded at the smallest size that's sharp at the current zoom, so large boards stay light.

## The CLI

```sh
mood new references              # create a board and make it current
mood add ~/Downloads/poster.jpg  # or a URL; a page URL uses its og:image
mood note 12 "+ this palette"    # + likes, - dislikes, anything else is a note
mood ls                          # every item: id, cell, rating, size, title
mood dump                        # every item and note as markdown
mood view C3                     # render a cell, a range (B2:D4) or #12 to PNG
```

`mood --help` lists every command. Boards live in `~/moodboards`, or in `$MOOD_HOME` if it's set.

## Chrome extension

`extension/` adds **Add to mood** to Chrome's right-click menu, with a submenu of your boards.

1. Open `chrome://extensions` and turn on Developer mode.
2. Click **Load unpacked** and choose the `extension/` folder.
3. Reload any tabs that were already open.

The extension downloads each image with your Chrome sign-ins, so images from logged-in sites work. It grabs the largest version the page offers and finds images hidden under overlays or set as CSS backgrounds. On Google Images, it records the result's own site as the source rather than the search page. It needs `mood serve` running.

## Security

The server only listens on 127.0.0.1. Writes need an `x-mood` header, which other websites can't send without the server's permission, and it never gives that permission. It also refuses requests whose Host isn't `localhost`, which stops another site from pointing its own domain name at your machine to get around that.

## License

MIT

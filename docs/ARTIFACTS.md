# Artifacts

Artifacts are small web apps built inside Studio. Each one is an ordinary folder of HTML, CSS and
JavaScript with its own data, its own window, and, once published, a permanent public URL on GitHub
Pages. Nothing about an artifact needs an account to view, and nothing about a published artifact
needs Studio to keep running.

## Where artifacts live

Each artifact is a folder under the Studio config directory:

```
<studio config>/artifacts/<slug>/
```

`index.html` is the entry point. Editable files are HTML, CSS, JavaScript, JSON, Markdown, SVG and
text. Images and fonts are allowed when an artifact is published, but the editor only shows text.
A file can be at most 1 MB and at most two folders deep (`pages/about.html` is fine,
`a/b/c.html` is not). Dotfiles and paths that climb out of the folder are refused.

The database keeps the identity: name, slug, description, template, timestamps, publish time and
the live URL. The folder is the source of truth for the code.

## Create and edit

**Code > Artifacts > New artifact** asks for a name, a description and a starting point:

- **Blank** is a single page with a counter saved through the runtime.
- **To-do** is a filtered task list with counts and export.
- **Notes** is a note list that autosaves as you type.

The editor shows one file at a time with tabs. Ctrl+S saves. Unsaved changes are marked on the tab
and in the footer, and switching files or artifacts saves first so nothing is silently lost. New
file writes a starter file for the extension you chose.

## Preview

**Code > Artifacts** has a Preview and a Code view. Preview renders the saved files inside Studio,
in a frame served by the same loopback HTTP server that **Open window** uses. The server binds
`127.0.0.1` on a random port, requires an unguessable token in the URL, serves only files inside the
artifact folder, and never lists directories. Responses are capped at 8 MB and carry `no-store`,
`nosniff` and `no-referrer`. HTML responses also carry a `frame-ancestors` policy that allows only
Studio windows.

The preview server injects one extra script, `/<token>/_customizer.js`, into every HTML page it
serves. Publish copies the folder as it is, so the public site never carries the script. The script
does nothing until Studio switches it on, which is why the preview window and the installed app keep
working normally.

A draft can be previewed before it is published; previews never touch the public site. Saving a file
reloads the frame, so the preview always shows the saved folder.

## Customize with AI

The Customizer panel sits beside the preview. The loop is built so the model proposes and Studio
decides:

1. **Select element** turns the preview into pick mode. Clicking any element highlights it and sends
   its tag, id, classes, selector, visible text and outer HTML to Studio. Clicking a link or a button
   in pick mode selects it instead of activating it. Escape turns pick mode off.
2. Describe the change and pick an agent. The installed headless agents are Claude Code,
   Antigravity and Hermes; the default is the strongest one installed here.
3. The agent runs one headless turn in the artifact folder with read-only tools and answers with a
   JSON plan: a summary and up to 12 edits, each an exact `old` text and its `new` replacement. With
   no element selected the request still works and the agent edits from the description.
4. Studio validates the plan before showing it: every `old` must match the current file exactly and
   uniquely, paths must stay inside the artifact, `agentos.js` is refused, new files start empty, and
   every replacement runs the same rules as the publish check (no placeholder markers, no banned
   dashes, no localhost URLs).
5. The panel shows the exact patch per file with its line and reason. **Apply changes** re-reads the
   files, validates the whole plan again against the disk, writes each touched file once and reloads
   the preview. **Discard** writes nothing.

The agent never writes files itself. It receives the file contents and returns text; Studio performs
every write after the review. If you have unsaved editor changes, the customizer asks you to save
first, because the plan was validated against the saved files. A stale or ambiguous patch is refused
with a message to generate it again. The customizer runs no approval prompt, like any other local
edit; publishing still asks.

## Install as an app

**Install as app** opens the artifact's public URL in its own window and remembers the choice.
Every Studio start reopens installed artifacts in their own windows, sized 1120 by 800 with a 420
by 320 minimum. Install needs a published artifact because the window loads the live site, so the
app keeps working when Studio is closed. **Uninstall** stops reopening the window; the published
site is untouched.

## Publish

**Publish** first runs the publish check on the saved files, then commits every published artifact
plus the current one to one GitHub repository and enables GitHub Pages on it. The default site is
`<your GitHub login>/agentos-artifacts`. Set a different repository in **Site**; it must belong to
the signed-in account. The repository is created (public) on the first publish if it does not exist.

### Publish check

The header shows **Ready to publish** or a count of problems. The button opens the list with file
and line. The check reads the saved folder and refuses the publish when it finds:

- index.html missing or empty;
- unbalanced HTML: an opening tag never closed, a close tag with no opening tag, or a tag that never
  ends with `>`. Script, style, textarea and title bodies are skipped, so comparisons and template
  strings do not count as tags;
- a local `src`, `href` or CSS `url()` that does not exist, or one written from the site root
  (`/styles.css`), which breaks under `/a/<slug>/`;
- an empty `src` or `href`;
- a file larger than 1 MB;
- a JavaScript file that `node --check` rejects, when Node is installed; module syntax is skipped;
- a placeholder marker (`TODO` or `FIXME` as a whole word, `implement later`, `lorem ipsum`);
- an em dash or en dash;
- `localhost`, `127.0.0.1` or `0.0.0.0`;
- text that is not UTF-8.

The list is capped at 60 problems. Publish names the first problems in its error, so a broken page
cannot reach GitHub Pages. The same rules run on every customizer replacement, which is what keeps a
vibe-coded patch out of the site.

The site layout:

```
index.html                 gallery of published artifacts, with search
404.html
manifest.json              machine-readable list of the published set
runtime/agentos.js         storage helpers for the gallery
a/<slug>/                  one folder per published artifact, index.html inside
.github/workflows/pages.yml  deploy workflow (configure, upload, deploy Pages actions)
.nojekyll
.agentos-artifacts.json    ownership marker
```

Each publish starts from a fresh clone of the site repository, syncs the folders of published
artifacts, removes folders for artifacts that are no longer published, rewrites the gallery, and
pushes one commit. Drafts never upload. Deleting an artifact locally leaves its published copy
online until the next publish, which removes it from both the site and the gallery.

The first publish enables Pages with the Actions build type. If the repository already has Pages on
a legacy branch build, Studio switches it to the workflow build and keeps any custom domain and
HTTPS setting. A deploy usually appears within a minute of the push; **Site** shows the last run
and links to its log.

Publishing needs `gh` signed in on this machine. Studio runs git with `GIT_TERMINAL_PROMPT=0` and
`GCM_INTERACTIVE=never`, so a credential problem fails loudly instead of hanging on a prompt.

### Safety of the site repository

Studio refuses to publish into a repository that holds content it did not create. The first
publish writes `.agentos-artifacts.json`; any later publish into a clone without that marker fails
before anything is pushed. Point the site setting at a dedicated repository, not at a repository
with your own site in it.

## Custom domain

**Site > Custom domain** points Pages at a host you own. Add a CNAME record for that host at your
DNS provider pointing at your GitHub Pages address first, then save the domain in Studio. Studio
sends the domain and keeps HTTPS enforcement on. Clearing the field removes the custom domain.

## Data and storage

Every artifact gets `agentos.js` when it is created, and publishing writes it again. It is
generated: the editor shows it read-only, and Studio rewrites it on every publish. It exposes
`window.AgentOS`:

| Call | What it does |
| --- | --- |
| `AgentOS.get(name, fallback)` | Read a saved value as JSON |
| `AgentOS.set(name, value)` | Save a value as JSON and return it |
| `AgentOS.remove(name)` | Remove one value |
| `AgentOS.all()` | Every value saved by this artifact |
| `AgentOS.clear()` | Remove every value saved by this artifact |
| `AgentOS.exportData()` | A JSON string with artifact name, export time and data |
| `AgentOS.importData(text)` | Restore values from an export |
| `AgentOS.download(filename, text)` | Save a text file through the browser |
| `AgentOS.fetchJSON(url, options)` | Fetch and parse JSON with a status check |

Values are stored in the browser's `localStorage` under keys prefixed with the artifact slug, so two
artifacts never collide. The `data-artifact` attribute on `<html>` sets the prefix; keep it in the
page. Data belongs to the browser: preview, the installed window and the public site each have their
own storage, and export/import moves data between them. An artifact hosted on the public site has
no access to Studio or to the fleet; it is an ordinary static page.

## Permissions

Three actions ask before they run, and each can be set to Allow in Safety:

- `artifacts.publish` (medium): commit and push to the public site.
- `artifacts.domain` (high): change the custom domain through GitHub Pages.
- `artifacts.delete` (medium): delete the artifact and its local files.

Local editing, preview and install do not ask.

## Limits and failures to expect

- **Pages 404 right after publishing.** The first deploy runs the workflow; wait for the run in
  **Site** to finish.
- **Push rejected.** The repository changed outside Studio. Publish again; Studio resets to the
  remote `main`, keeps the marker check, and rebuilds the site from the published set.
- **Marker refused.** The site repository was not created by Studio. Choose a different repository.
- **Repository must belong to the signed-in account.** Studio checks the owner against `gh` before
  saving the site setting.
- **Rate limits.** Many publishes in a row can hit GitHub API limits; space them out.
- **Large artifacts.** Each editable file is capped at 1 MB and the whole artifact is copied into
  the site on every publish, so keep it small. That is the design: a fast static site, not a file
  host.
- **Node is not installed.** The publish check still runs every static rule, but it cannot run
  `node --check`, so JavaScript syntax is not verified until Node is on PATH.
- **Customizer agent unavailable.** The panel names the three headless agents. Install one of them,
  or the request is refused before any turn starts. A plan also names what failed when the patch is
  stale, ambiguous or touches a generated file.

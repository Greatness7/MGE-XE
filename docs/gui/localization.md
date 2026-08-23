# MGEXEgui localization

MGEXEgui embeds its user-interface translations in the executable with
`rust-i18n`. The shipped catalogs are:

```text
MGEXEgui/locales/en.toml
MGEXEgui/locales/fr.toml
MGEXEgui/locales/pl.toml
MGEXEgui/locales/ru.toml
```

English is the source catalog and fallback. There is no runtime `.lng`
parser, external language-pack discovery, or live catalog reload.

## Locale selection

The persisted preference is `gui.language` in game-root `mgeXE.toml`; see the
[configuration reference](../configuration/mge-toml.md). Its value is either
`auto` or one of the embedded locale codes.

At initial startup, before settings are available, the GUI selects:

1. the non-persisted `MGEGUI_LOCALE` environment override, when valid;
2. the best embedded match for the operating-system locale; or
3. English.

After settings load, a valid manual choice takes precedence over the system
locale. An invalid or unavailable saved value is normalized to `auto`.
Regional forms such as `fr-FR` and underscore forms such as `pl_PL` first try
an exact catalog and then reduce to the language code.

Changing the selector applies the locale immediately. Reload, import, and reset
operations reapply locale selection after replacing the live settings; reset
restores `auto`. The selector displays each manual choice using its catalog's
self-localized `language.name`.

## Catalog contract

Application-owned text uses stable semantic keys grouped by UI area, such as
`tabs.distant_land`, `shaders.editor.save_prompt`, or
`generator.progress.optimize_meshes`. English sentences and former WinForms
control names are not keys.

Translate complete phrases at the presentation boundary with
`rust_i18n::t!`. Interpolated values use named `%{value}` placeholders, and
every translation of a key must preserve the English placeholder set. Do not
build sentences by concatenating translated fragments.

Production keys must exist in every secondary catalog unless they are listed
in the explicit intentional-fallback allowlist in
`MGEXEgui/src/localization.rs`. Missing permitted entries resolve to English,
never to a raw key or blank string.

## Scope

Localize application-owned:

- labels, buttons, headings, cards, tabs, menus, and tooltips;
- window titles, dialogs, toasts, and instructions;
- progress stages and application-owned error/status framing; and
- display names for weather, enum choices, and other UI concepts.

Keep technical or user-owned data unchanged:

- plugin and shader filenames;
- filesystem paths, key names, and console commands;
- shader source and log contents; and
- operating-system, I/O, or dependency-produced error details.

For errors, translate the surrounding actionable message and interpolate the
original technical detail. Background workers send stable stage values or keys;
translation happens when the UI renders them, so worker results are independent
of the currently selected locale.

## Adding or changing text

1. Add the semantic key and English wording to `locales/en.toml`.
2. Add the same key to French, Polish, and Russian, preserving named
   placeholders.
3. Call `rust_i18n::t!` at the UI presentation boundary.
4. If a temporary English fallback is intentional, add only that key to the
   test allowlist and record why.
5. Run `cargo test -p MGEXEgui` and a localized UI survey when layout could
   change.

Catalog tests verify that secondary locales have no unexpected missing or extra
keys, named placeholders match English, every locale has a self-localized name,
regional locale reduction works, and intentional fallback resolves to English.
`mge-config` tests cover the `auto` default and targeted-save round trip.

For visual checks, launch the GUI under `MGEGUI_LOCALE`:

```powershell
$env:MGEGUI_LOCALE = 'ru'; cargo run -p MGEXEgui
```

Use Russian to stress glyph coverage and width, French to stress longer text,
and Polish for a spot check. The GUI never persists this override.


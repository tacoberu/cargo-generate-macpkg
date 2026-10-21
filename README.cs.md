cargo-generate-macpkg
=====================

[English](README.md) | **Česky**

Pomocný příkaz Cargo, který z Cargo projektu vytvoří instalační balíček pro macOS (`.pkg`).
Nepotřebuje Xcode: obsah (`Payload`), instalační skripty a `PackageInfo` zapisuje nástroj sám,
a na Linuxu seznam souborů (`Bom`) a archiv vytvoří `mkbom` a `xar`.
Inspirováno [cargo-generate-rpm](https://github.com/cat-in-136/cargo-generate-rpm).

## Instalace

```
cargo install --path .
```

## Použití

Spouští se v Cargo projektu, který chceš zabalit, po přeložení binárek:

```
cargo build --release --target aarch64-apple-darwin          # na Macu
cargo generate-macpkg --target aarch64-apple-darwin
```

Výsledek je `target/generate-macpkg/<název>-<verze>-<arch>.pkg`, kde `<arch>` je `arm64`, `x86_64`, …
(podle `--target`, bez něj `any`). Výstup lze změnit přepínačem `-o` (soubor nebo adresář).
Přepínače `--target` a `--profile` musí odpovídat tomu, jak byly binárky přeloženy; příkaz je
nepřekládá ani balíček nepodepisuje. Ve workspace příkaz spusť v adresáři balíčku, nebo balíček
vyber přepínačem `-p NAZEV_NEBO_CESTA`.

Jak se balíček skládá (`--tool`):

| Nástroj | Kde | Potřebuje |
|---|---|---|
| `pkgbuild` | macOS (tam výchozí) | `pkgbuild` z Command Line Tools |
| `xar` | Linux (výchozí bez `pkgbuild`) | `mkbom` z [bomutils](https://github.com/hogliux/bomutils) a `xar` (Fedora: `dnf install xar`) |

Cesty k programům jde zadat přepínači `--pkgbuild`, `--mkbom` a `--xar`.

Binárky se dají z Linuxu přeložit přes [`cargo-zigbuild`](https://github.com/rust-cross/cargo-zigbuild)
(`cargo install cargo-zigbuild`, vyžaduje `zig`). Balíček **není podepsaný ani notarizovaný**; to
udělají `productsign` a `notarytool` na Macu.

## Konfigurace

Nastavení je v `Cargo.toml`, v tabulce `[package.metadata.generate-macpkg]`.

| Volba | Povinná | Výchozí |
|---|---|---|
| `identifier` | **ano** | identifikátor ve tvaru reverzní domény, např. `org.example.tool` |
| `name` | ne | `package.name` (název souboru balíčku) |
| `version` | ne | `package.version` |
| `install_location` | ne | `/` |
| `assets` | **ano** | soubory v obsahu balíčku, viz níže |
| `scripts` | ne | adresář s instalačními skripty `preinstall`, `postinstall` (všechny soubory se nastaví jako spustitelné) |

`assets` je pole položek se `source`, `dest` a `mode` (osmičkový řetězec, výchozí `"0644"`). `dest` je
absolutní cesta na cílovém svazku (`/usr/local/bin/tool`); končí-li `/`, přidá se název souboru ze
`source`. Obsahuje-li `source` hvězdičku `*`, musí `dest` končit `/`. Prefix `target/release/` ve
`source` se nahradí skutečným adresářem překladu podle `--target` a `--profile`. Nadřazené adresáře
se vytvoří (`root:wheel`, `0755`); soubory patří `root:wheel`.

Funkční příklad je v [`examples/macdemo`](examples/macdemo).

## Stav

Obsah (archiv `cpio`), archiv skriptů, `PackageInfo` a volání `pkgbuild`, `mkbom` a `xar` pokrývají
testy (samotné programy zástupci). Balíček složený skutečnými `xar` a `mkbom` a jeho instalace na
Macu **nejsou ověřené**.

## Licence

MIT, viz [LICENSE](LICENSE).

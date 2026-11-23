# Wspólny workspace i Filen

Agenci pracują w tym samym katalogu projektu na PC. Wspólne pliki służą
do wymiany wyników, załączników i materiałów pomocniczych. Kod
wersjonujemy w Git. Wybrane pliki można opublikować do osobnego katalogu
synchronizowanego z chmurą.

## Konfiguracja na PC

Po zbudowaniu lub zainstalowaniu `octo` uruchom poniższe komendy w
głównym katalogu projektu. W projekcie z istniejącym `octomesh.toml`
zacznij od `octo workspace init`.

```sh
octo init
octo workspace init
octo workspace info
```

Wynik zawiera trzy ścieżki.

| Pole | Przeznaczenie |
|---|---|
| `project_root` | Projekt rozpoznany po najbliższym `octomesh.toml` w bieżącym repozytorium |
| `files` | Lokalny katalog roboczy `.octomesh/workspace/files` dostępny dla obu terminali |
| `exports` | Katalog `.octomesh/workspace/exports` przeznaczony do publikacji wyników |
| `inbox` | Katalog zweryfikowanych wersji pobranych z innych maszyn przez backup |

`octo workspace info` działa również z podkatalogów. Klient MCP
uruchomiony w projekcie może wywołać `mesh_workspace` i odczytać
ścieżkę `files`. Uruchamiaj serwer MCP z katalogu głównego projektu.
Przy starcie z podkatalogu wskaż jawnie lokalny `octomesh.toml` przez
`--config` oraz plik `.octomesh/state.db` przez `--db`. W kliencie
uruchomionym przed inicjalizacją odśwież serwer MCP.

Po zakończeniu pracy nad plikiem, przykładowo
`files/ifc/komin-r02.ifc`, opublikuj go z dowolnego podkatalogu projektu.

```sh
octo workspace publish ifc/komin-r02.ifc
```

Polecenie kopiuje plik do `exports/komin-r02.ifc`. Katalog eksportu
otrzymuje kompletną kopię pod nazwą pliku. Istniejąca nazwa daje błąd.
Zapisuj kolejne rewizje z nowymi nazwami, np. `komin-r03.ifc`. Katalog
workspace jest ignorowany przez Git. Publikacja używa dowiązań twardych
w lokalnym systemie plików, np. ext4 lub NTFS.

## Filen Desktop

1. Zaloguj się w kliencie Filen na PC i dodaj parę synchronizacji.
2. Jako lokalny katalog wybierz dokładnie `exports` z
   `octo workspace info`. W oknie wyboru wklej pełną ścieżkę, ponieważ
   katalog jest pod ukrytą nazwą `.octomesh`.
3. W chmurze utwórz osobny katalog dla projektu. Wybierz tryb
   **Local Backup**, który przesyła pliki z PC do Filen i zachowuje
   zdalne kopie przy lokalnym usuwaniu. Sprawdź tryb w używanej wersji.
4. Opublikuj niewielki plik testowy i sprawdź status synchronizacji
   oraz pobranie w aplikacji Filen na drugim urządzeniu. Sukces
   `octo workspace publish` oznacza ukończenie lokalnego eksportu.
   Status wysłania do chmury pokazuje klient Filen.

Filen obsługuje swoje logowanie i szyfrowanie. OctoMesh działa offline
i przygotowuje eksporty także podczas przerwy w łączności. SQLite,
konfigurację, pliki agentów i sesje terminali trzymaj poza wybraną parą
synchronizacji. Do katalogu `exports` trafiają wybrane przez operatora
artefakty. Pliki pobrane z chmury można ręcznie umieścić w `files`.

Dokumentacja dostawcy jest w [Filen Desktop](https://github.com/FilenCloudDienste/filen-desktop)
i [Filen Sync](https://github.com/FilenCloudDienste/filen-sync).

## Internxt PC ↔ VPS

`octo workspace backup --watch` automatycznie wymienia wersje ścieżek z
`files` przez zaszyfrowany zdalny katalog rclone. Konfigurację i układ
projektów opisuje [wymiana przez Internxt](internxt-backup.md).

### Ręczne wysłanie jednego pliku

Jeśli używasz Internxt, ten sam kompletny eksport można wysłać przez
oficjalne CLI. Po zalogowaniu w aplikacji lub CLI użyj ścieżki zwróconej
przez publikację i identyfikatora katalogu projektu.

```sh
internxt upload-file --file "<ścieżka z octo workspace publish>" --destination "<ID katalogu>"
```

Polecenie i obsługiwane opcje są opisane w [Internxt CLI](https://github.com/internxt/cli#internxt-upload-file).

# Zaszyfrowana wymiana przez Internxt między PC i VPS

OctoMesh robi kopie stabilnych plików z `.octomesh/workspace/files`.
Po zapisaniu lokalnego snapshotu wysyła go przez `rclone crypt`. Druga
maszyna pobiera wersję, sprawdza SHA-256 i dopiero wtedy pokazuje ją w
`.octomesh/workspace/inbox`. Aktywny katalog pracy i odebrane wersje
mają osobne ścieżki, więc dwa równoległe zapisy zachowują oba wyniki.
Usunięcie pliku roboczego nie usuwa zdalnej kopii.

## Układ projektu

Poniżej logiczne nazwy widoczne przez zdalny katalog `crypt`.

```text
OctoMesh/
└── Projekty/
    └── p_123/
        ├── Repozytoria/
        │   └── spatial-cad/
        │       └── Maszyny/pc/Agenty/codex/Sesje/r01/Wersje/<id>/
        └── Maszyny/
            ├── pc/Agenty/codex/Sesje/r01/Wersje/<id>/
            └── vps/Agenty/vibe/Sesje/r02/Wersje/<id>/
```

Pliki zapisane pod `files/Repozytoria/<repo>/` trafiają do gałęzi tego
repozytorium. Pozostałe pliki trafiają do gałęzi maszyny i sesji.
Wersja zawiera `payload` oraz `manifest.json` z pierwotną ścieżką,
rozmiarem, czasem modyfikacji i SHA-256. Ten sam zapisany stan pliku
w danym strumieniu ma ten sam identyfikator.

Każda maszyna widzi odebrane pliki w
`inbox/<projekt>/<maszyna>/Agenty/<agent>/Sesje/<sesja>/<id>/files/`.
`mesh_workspace` podaje tę ścieżkę bazową agentom. Przeniesienie
odebranego pliku do `files` pozwala przyjąć go do dalszej pracy.

Identyfikatory projektu i katalog chmurowy są wspólne na obu
maszynach. Identyfikatory maszyn są różne. Nazwy agenta i sesji
etykietują strumień przypisany przy uruchomieniu. Jeden proces
monitoruje jeden współdzielony katalog, domyślnie jako
`Agenty/wspolne/Sesje/workspace`.

## 1. Skonfiguruj Internxt i rclone na obu maszynach

Na PC i VPS wybierz rclone, w którym działa `rclone help backend internxt`.
Potwierdzona wersja z takim backendem to rclone 1.75.2.
W `rclone config` utwórz dwa wpisy.

1. `internxt-drive` typu `internxt`. Zaloguj się przez interaktywną
   konfigurację rclone i podaj kod 2FA, jeśli go używasz. Sprawdź dostęp
   i pojemność poleceniem `rclone about internxt-drive:`. Zgodność planu
   lifetime 5 TB z tym backendem potwierdza ten test na Twoim koncie.
2. `internxt-crypt` typu `crypt` na `internxt-drive:OctoMeshCipher`.
   Wybierz standardowe szyfrowanie nazw oraz szyfrowanie nazw katalogów.
   Na obu maszynach użyj tego samego hasła i tej samej soli dla
   `crypt`, z odtwarzalną kopią kluczy w bezpiecznym miejscu.

Logowanie i klucze zostają w lokalnej konfiguracji rclone. OctoMesh
przyjmuje nazwę zdalnego katalogu, sprawdza możliwość szyfrowania przez
`rclone backend encode` i wymusza szyfrowanie zawartości, plików i
nazw katalogów podczas transferu. W panelu Internxt widać zaszyfrowane
obiekty. Logiczną strukturę przeglądaj przez
`rclone lsf internxt-crypt:OctoMesh/Projekty/p_123`.

### Wariant z lokalnym WebDAV

Jeśli konto i wersja klienta obsługują WebDAV, możesz użyć
`internxt-dav:OctoMeshCipher` jako zaplecza tego samego typu `crypt`.
Najpierw zaloguj się w oficjalnym Internxt CLI i ustaw bramkę na
lokalnym interfejsie.

```sh
internxt login
internxt webdav-config --host 127.0.0.1 --https
internxt webdav enable
internxt webdav status
```

Wpis `internxt-dav` w rclone powinien mieć typ WebDAV, dostawcę
`other` i adres odpowiadający certyfikatowi lokalnej bramki.
Zaufaj jej certyfikatowi w magazynie systemowym albo podaj lokalny
CA w konfiguracji rclone. Wbudowany backend `internxt` działa
bez tej bramki.

Źródła [Backend Internxt w rclone](https://rclone.org/internxt/) · [rclone crypt](https://rclone.org/crypt/) · [Internxt WebDAV](https://github.com/internxt/cli/blob/main/WEBDAV.md).

## 2. Uruchom dwa strumienie

Zainicjalizuj workspace w każdym lokalnym checkoutcie projektu.
W istniejącym projekcie z `octomesh.toml` wystarczy
`octo workspace init`. Na PC uruchom:

```sh
octo workspace backup --remote internxt-crypt:OctoMesh \
  --project p_123 --machine pc --watch
```

Na VPS użyj tych samych parametrów i ustaw `--machine vps`.
`--agent codex --session r01` można dodać do strumienia przypisanego
do konkretnej sesji. Bez `--watch` narzędzie wykonuje jeden przebieg.

Proces sprawdza zapisane pliki co 15 s. Domyślnie bierze pliki od
co najmniej 2 s bez modyfikacji. Katalogi i pliki ukryte, dowiązania
i tymczasowe rozszerzenia `.tmp`, `.part` oraz `~` są pomijane.
Kolejka lokalna przechowuje pełne snapshoty do potwierdzenia wysyłki.
Wysyłka i pobrania są ponawiane w kolejnych przebiegach. W razie
przerwy w sieci potrzebne jest miejsce na nieprzesłane dane. Odbiornik
pobiera wersje pozostałych maszyn w wybranym projekcie i zachowuje je
lokalnie.

Na Linuksie można uruchamiać proces jako usługę użytkownika. Przykładowy
plik `~/.config/systemd/user/octomesh-backup-p_123.service`:

```ini
[Unit]
Description=OctoMesh encrypted exchange for p_123

[Service]
WorkingDirectory=/abs/path/to/project
ExecStart=/abs/path/to/octo workspace backup --remote internxt-crypt:OctoMesh --project p_123 --machine pc --watch
Restart=on-failure
RestartSec=10

[Install]
WantedBy=default.target
```

Ustaw identyfikator właściwej maszyny. Usługa musi mieć dostęp do
`rclone` w PATH. W wariancie WebDAV uruchom bramkę na tym samym
hoście. Włącz wymianę poleceniem
`systemctl --user enable --now octomesh-backup-p_123.service`.
Na VPS usługi użytkownika mogą wymagać włączenia pracy bez sesji
logowania. Zaszyfrowana konfiguracja rclone wymaga nieinteraktywnego,
lokalnego mechanizmu dostarczania jej hasła.

## 3. Odtwórz wybraną wersję

Każda odebrana i zweryfikowana wersja jest już w katalogu `inbox` z
oryginalnym zagnieżdżeniem. Gdy odzyskujesz dane bez lokalnej kolejki,
użyj rclone z tym samym wpisem crypt. Poniższe polecenie pobiera zawartość
wskazanej wersji do osobnego katalogu.

```sh
rclone copy 'internxt-crypt:OctoMesh/Projekty/p_123/Maszyny/pc/Agenty/wspolne/Sesje/workspace/Wersje/<id>' ./odzysk
sha256sum ./odzysk/payload
```

Porównaj sumę z `sha256` w `manifest.json`. Manifest wskazuje też
oryginalną nazwę i miejsce pliku.

## Odbiór lokalny

`cargo build --locked -p octo-cli` buduje program. Następnie uruchom
`python3 tools/check-internxt-backup.py` z rclone w PATH. Test używa
lokalnego WebDAV przez pętlę zwrotną i sprawdza rzeczywiste szyfrowanie
rclone, kolejkę offline, równoległe wersje PC/VPS oraz tryb `--watch`.

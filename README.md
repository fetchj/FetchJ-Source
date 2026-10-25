# FetchJ – source for review / código para revisión

FetchJ downloads a lawyer's own case files from the online system of the courts, on the lawyer's own computer.
This repository shows the parts of FetchJ that **talk to the outside world**, so that anyone can check what the
program does with logins, passwords and case files.

**ES:** FetchJ descarga los expedientes del propio abogado desde el sistema en línea de los tribunales, en su propia
computadora. Este repositorio muestra las partes de FetchJ que **se comunican con el exterior**, para que cualquiera
pueda comprobar qué hace el programa con sesiones, contraseñas y expedientes.

## What is in here / Qué contiene

| Folder / file | What it shows |
|---|---|
| `src/portal.js`, `src/login.js`, `src/bridge.rs` | the script inside the portal window, the login assistant (it never types or reads a password), the bridge between the window and the program |
| `src/portalsess.rs` | how long a portal login is kept and how FetchJ logs out |
| `src/run/` | the update: case list, documents, downloads, recordings, keeping the session alive |
| `src/nexus.rs` | the search of published judgments (case law) |
| `src/log.rs`, `src/privacy.rs` | what is written into the logs, and how tokens, cookies and personal data are removed from them |
| `src/totp.rs`, `src/aiapi.rs` | how an authenticator key and an AI API key are stored (Windows data protection, never in a settings file or log) |
| `src/anon.rs`, `src/aipack.rs`, `src/aipdf.rs`, `src/aiprompt.rs` | the AI handover: what is anonymised, what leaves the computer, and only when the user starts it |
| `src/country.rs`, `data/countries/` | addresses and texts of the portal per country |

## What is not in here / Qué no contiene

- **Validation and licence checks are removed** (marked in the code). / **Las comprobaciones de validación y de licencia
  se eliminaron** (marcadas en el código). / **Validierungs- und Lizenz-bedingte Prüfungen sind im öffentlichen
  Quellcode entfernt** (im Code markiert).
- Comments, tests and the user interface are removed. The code does not compile as it is.

## AI handover / Entrega a la IA

FetchJ hands case files to an AI only when the user starts it, and shows everything before it leaves the computer.
**The user is responsible** for whether case files may be given to an AI (professional secrecy, data protection,
consent of the clients). / **El usuario es responsable** de si puede entregar expedientes a una IA (secreto
profesional, protección de datos, consentimiento de sus clientes).

## Is it the code of the installer? / ¿Es el código del instalador?

`SOURCE_INFO.txt` names the FetchJ version and the commit of the private repository the installer was built from.
`MANIFEST.sha256` lists a SHA-256 for every file. Each version has its own tag here (`v0.9.3` …).

## Licence / Licencia

Read and review only – see `LICENSE` (FetchJ Source Review License 1.0, Mute Consult LLC).
Solo lectura y revisión – ver `LICENSE`.

Independent product – not a product of the courts. / Producto independiente – no es un producto del Poder Judicial.

# Nucleus

Nucleus contains a Rust backend, the Halo website, deployment tools, and isolated test fixtures. Start with [the documentation index](docs/README.md) for implemented behavior, architecture, and remaining work.

| Path                 | Responsibility                                                                  |
| -------------------- | ------------------------------------------------------------------------------- |
| `src/`, `tests/`     | Application assembly, HTTP adaptation, and executable tests                     |
| `domain/`            | Business facts, use cases, and database protections consumed by the application |
| `platform/`          | Database, cryptography, and other technical capabilities                        |
| `deployment/`        | Migrations, database preparation, and authorized maintenance commands           |
| `fixture/`           | Disposable PostgreSQL and supplier fixtures                                     |
| `assets/`            | Runtime assets embedded in Rust binaries, including default avatars             |
| `halo/`              | Independently built website, protocol generation, and browser acceptance tests  |
| `tools/`, `.github/` | Repository validation tools and CI                                              |
| `docs/`              | Authoritative architecture, business contracts, and engineering rules           |
| `design/`            | Client page prototypes and their supporting design assets                       |

The client code names and reserved directory names are:

| Code name | Client              |
| --------- | ------------------- |
| `orbit`   | WeChat mini-program |
| `comet`   | iOS app             |
| `nova`    | Android app         |
| `halo`    | Website             |

These names remain reserved even while their directories do not exist. iOS and Android implementation is not currently planned.

The [Orbit page prototype](design/orbit-pages.html) is design material. The root `package.json` and `bun.lock` support repository tools and documentation checks, not a client application.

The website lives in `halo/` with its own `package.json` and `bun.lock`; its stack is described in [website engineering](docs/client/website.md). Follow [Halo's build and isolated verification instructions](halo/README.md) to run it. The website's user-facing name remains 司籍处. Its [Halo prototypes](docs/client/pages.md#网站-ui-设计) cover the public homepage, login and registration, and employee service home. Halo consumes the shared design tokens, logo, and botanical artwork; prototype scripts remain design material.

See [validation scope](docs/engineering/verification.md#仓库与检查归属) for the checks owned by each area and [delivery](docs/engineering/delivery.md) for the branch and PR workflow.

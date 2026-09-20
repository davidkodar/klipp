# Security Policy

## Supported versions

Klipp is pre-release. There are no supported versions yet: fixes land
on `main` and go out in the next tag. When the first stable release
has users, this section will say which versions get fixes and for how
long.

## Reporting a vulnerability

**Please do not open a public issue.**

Use GitHub's private vulnerability reporting — the **Security** tab on
this repository, then **Report a vulnerability**. It goes only to the
maintainer and lets us discuss and fix the problem before anything is
public.

Please include what you would want to receive: what the bug is, how to
reproduce it, what an attacker gets out of it, and the version or
commit you found it on. A proof-of-concept file is welcome; a working
weaponised exploit is not necessary and not wanted.

You will get an acknowledgement. Klipp is a single-developer project,
so a fix may not be fast, but you will be told where it stands rather
than left guessing. If you want credit in the release notes, say so —
and if you would rather not be named, that is fine too.

## Where the interesting attack surface is

If you are looking, these are the parts that take input from outside
and are therefore worth your time:

* **Project import** — MLT, OTIO and FCPXML files come from elsewhere
  and are parsed as untrusted. Paths route through
  `klipp_project::path_util::safe_resolve`, XML through
  `parse_xml_safe`, and reads through `read_to_string_capped`. A path
  escaping the project root, an XML entity expansion, or an
  unbounded read that gets past those is a real bug.
* **Media decode** — Klipp hands media files to FFmpeg. A crash in
  FFmpeg itself belongs upstream, but a crash in how Klipp drives it,
  or in what it does with the decoded output, is ours.
* **Plugin hosts** — the OFX and frei0r hosts `dlopen` shared objects
  from the user's plugin directories. Those are trusted by definition
  (they are native code the user installed), but the FFI boundary
  should not be turnable into a crash by a *malformed* plugin.
* **The control socket** — `klipp-control` listens on a Unix socket and
  accepts commands that edit the project, run analysers and write
  files. It is intended to be reachable only by the user who started
  Klipp. Anything that widens that, or that turns a socket command into
  a write outside the places it should write, is a bug worth reporting.
* **The model manifest** — models are downloaded on demand and verified
  by SHA-256 against `assets/models.toml`. A path by which a model file
  is used without that check passing is a supply-chain hole.

## What is out of scope

* Vulnerabilities in dependencies, unless Klipp's use of them is what
  makes the problem reachable. Report those upstream; tell us too if
  we need to pin or patch.
* Anything requiring an attacker who already has local code execution
  as the user running Klipp. At that point the editor is not the
  weakest thing on the machine.
* Missing hardening that is not exploitable on its own.

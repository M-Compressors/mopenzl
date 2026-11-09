# Security Policy

## Supported versions

mopenzl is pre-1.0. Security fixes land on `main` and ship in a new patch
release of the latest minor version.

## What counts as a vulnerability

Production code in the `mopenzl` crate forbids `unsafe`, so memory corruption
there should be impossible; a report that shows otherwise is especially
welcome. Please report privately anything that lets untrusted input cause:

- a panic, abort, or infinite loop in the decoder or any other API that
  accepts untrusted input;
- memory or CPU use beyond the limits a caller configured (`DecodeLimits`),
  such as a frame whose declared or intermediate sizes escape them;
- decoded output that silently differs from the original data without an
  error.

Encoder output that differs from the OpenZL reference but still decodes
correctly is a correctness bug rather than a vulnerability; open a regular
issue.

## Reporting

Use GitHub's private vulnerability reporting on the repository. Include the
input bytes (hex or a file), the API and configuration used, and the observed
behaviour.

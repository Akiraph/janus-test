# Web Application

The web app is the control plane's operator interface. Features follow capability ownership, so every visible state, command, stream update, and error has a clear owner; routing only connects URLs to the feature that owns them.

Do not reintroduce a forwarding `pages` layer, let components assemble HTTP requests, or maintain a second domain state machine. Cross-capability transport, errors, cursors, and generated types belong in `lib`; genuinely reusable visual primitives belong in `components`; business layout and interaction stay in features.

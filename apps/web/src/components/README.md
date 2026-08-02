# Shared UI

This directory contains UI primitives and interaction foundations that are independent of business terminology and genuinely reused across capabilities. Components should stay small and stable; they must not make Project, Session, or Turn decisions.

Feature-specific cards, panels, and layouts stay in their feature. A file that looks like a button or table is not automatically a shared component; repeated use and a stable interface are the reasons to abstract.

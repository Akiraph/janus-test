# Capability Features

Each feature should represent one operator capability, including its UI, interaction state, and necessary presentation logic. Project, Session, Execution, Runtime, and Model concepts follow backend ownership; cross-feature collaboration uses public queries and commands.

Do not create forwarding page shells, copy backend state machines, or centralize every request in one large component. A component that owns a broad layout still belongs to the feature that owns that layout rather than becoming a generic page layer.

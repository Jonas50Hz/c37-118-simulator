# Keep Scenario State Deterministic

Each PMU endpoint or PDC connection can have only one active fault scenario. Activation and sustained-scenario clearing use confirmed boundary-aligned operations, and observed host-clock recovery automatically restores normal time health at a reporting boundary without changing readiness.
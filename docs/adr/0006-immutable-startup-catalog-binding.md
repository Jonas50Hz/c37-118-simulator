# Bind an Immutable Scenario Catalog at Startup

An operator selects the scenario catalog through an explicit startup path alongside the PMU profile. The emulator validates and retains that catalog revision for its whole process lifetime, making runtime scenario activation deterministic and making the running behavior traceable by catalog SHA-256.
# Separate Scenario Behavior from PMU Profiles

Fault Scenarios live in separately versioned YAML catalogs referenced by startup profiles and use reporting-frame offsets for timing. This preserves deterministic runtime scenario control without duplicating PMU identity or wire configuration across test behavior definitions.
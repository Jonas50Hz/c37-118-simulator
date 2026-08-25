# Recover Through the Container Supervisor

The state-free emulator uses Docker Compose `restart: unless-stopped`. It is ready only after all configured listeners are bound, the shared Management Plane responds, and an internal protocol self-check for the selected wire version succeeds; external PDC connections never determine readiness.
# Isolate PDC and Recoverable Management Failures

The emulator disconnects a slow PDC without interrupting other PDCs, and uses separate initial-handshake and idle-session deadlines. Recoverable management errors return structured HTTP responses while streams continue; only inconsistent scenario state fails the process for Docker-supervised recovery.
# Serve The PMU Control Console Separately

The PMU Control Console is a separate Compose service for approved browsers on the trusted private routed network. It reads state and invokes confirmed Fault Scenario controls through the existing Management Plane, while PMU configuration and simulator lifecycle remain outside its authority.
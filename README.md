ESP32 AMP Sensor
================

This project is an embedded system for checking used amperes in a place.

Different adapters can be provided in the future to different purposes.

Current focus:
- Web server, since it's probably the easiest to test
- Publish presence through UDP multicast
- Client for this thing that can read presence through UDP multicast ask info to the web server and push some info to webhook on certain circumstances
- Different client that can push an actuator depending on certain circumstances
- Ability to configure via Wifi AP if STA not found, and save connection info to NVS
- Publish mDNS


E.g.,

- Pull-based
  - A local web server
  - A web server over a VPN (not sure if it would fit in the flash)
- Push-based
  - A webhook (web client) that can push current electricity usage over time with a certain periodicity
  - A MQTT client
  - A Redis client
- Matter
  - A Wifi/thread device implementation acting as a sensor

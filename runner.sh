#!/bin/bash

espflash --partition-table partitions.csv target/xtensa-esp32-espidf/debug/esp32-ota && \
esptool.py write_flash 0xd000 target/xtensa-esp32-espidf/debug/build/esp-idf-sys-*/out/build/ota_data_initial.bin && \
espmonitor /dev/ttyUSB0

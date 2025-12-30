#/bin/sh

set -e

#for p in `find ../Code-Examples/P00_perlin_noise/ -path "*/project" -type d -name project`; do
#  target/debug/harvest_translate --in-performer $p
#done

for p in `find ../Code-Examples/B01_synthetic/release/ -path "*/project" -type d -name project`; do
  target/debug/harvest_translate --in-performer $p
done

for p in `find ../Code-Examples/B01_organic/release/ -path "*/project" -type d -name project`; do
  target/debug/harvest_translate --in-performer $p
done
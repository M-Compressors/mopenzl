# Silesia comparison

**AMD Ryzen 9 7950X (KVM, 2 vCPUs) · rustc 1.99.1 · C at -O3 · full Silesia files · 2026-11-10.** Each standard graph compresses every listed Silesia file as one
input with the library defaults (level 6, format version 27, both checksums).
Every frame was verified byte-identical to OpenZL v0.3.0 C, and both decoders
restored the input, before timing; compressed sizes are therefore equal.
Speed ratio is C time / Rust time (above 1× means Rust is faster), the median
of alternating rounds. Throughput is of uncompressed bytes.

![Median speed ratio per graph](charts/overview.svg)

Raw measurements: [silesia-2026-11-10/](silesia-2026-11-10/) · [how to reproduce](../benchmarking.md#silesia-comparison).

## Medians per graph

| Graph | Files | Compress / C | Decompress / C | Before: compress / C | Before: decompress / C |
| --- | --- | --- | --- | --- | --- |
| zstd | 12 | 0.989× | 0.957× | 0.951× | 0.963× |
| lz4 | 12 | 0.954× | 0.918× | 0.784× | 0.305× |
| lz | 12 | 0.834× | 1.465× | 0.489× | 0.436× |
| huffman | 12 | 0.936× | 0.782× | 0.397× | 0.197× |
| fse | 12 | 0.955× | 0.881× | 0.437× | 0.294× |
| entropy | 12 | 0.896× | 0.774× | 0.401× | 0.198× |
| huffman_pivco | 12 | 1.053× | 1.026× | 0.092× | 0.051× |
| flatpack | 12 | 1.472× | 1.490× | 0.734× | 1.432× |
| generic | 12 | 0.976× | 0.960× | 0.972× | 0.947× |
| field_lz_struct4 | 3 | 0.827× | 0.763× | 0.598× | 0.323× |
| bitpack_num16 | 2 | 1.774× | 2.248× | 0.078× | 0.683× |
| numeric16 | 2 | 0.943× | 0.798× | 0.483× | 0.269× |
| numeric32 | 3 | 0.827× | 0.706× | 0.557× | 0.368× |
| generic_num16 | 2 | 0.878× | 0.826× | 0.508× | 0.282× |
| generic_num32 | 3 | 0.844× | 0.710× | 0.615× | 0.334× |

## zstd

![zstd](charts/zstd.svg)

| File | Bytes | Ratio | Rust compress | C compress | / C | Rust decompress | C decompress | / C |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| dickens | 10192446 | 2.982 | 97.0 MiB/s | 97.1 MiB/s | 0.998× | 1262.7 MiB/s | 1265.0 MiB/s | 0.998× |
| mozilla | 51220480 | 2.986 | 132.9 MiB/s | 136.6 MiB/s | 0.973× | 824.5 MiB/s | 874.0 MiB/s | 0.943× |
| mr | 9970564 | 2.944 | 125.2 MiB/s | 126.0 MiB/s | 0.994× | 1410.2 MiB/s | 1525.0 MiB/s | 0.925× |
| nci | 33553445 | 13.830 | 245.9 MiB/s | 251.8 MiB/s | 0.977× | 2314.4 MiB/s | 2803.0 MiB/s | 0.826× |
| ooffice | 6152192 | 2.099 | 107.4 MiB/s | 111.7 MiB/s | 0.962× | 1197.8 MiB/s | 1241.6 MiB/s | 0.965× |
| osdb | 10085684 | 2.970 | 144.7 MiB/s | 141.8 MiB/s | 1.021× | 2063.4 MiB/s | 1982.8 MiB/s | 1.041× |
| reymont | 6627202 | 3.735 | 111.2 MiB/s | 113.0 MiB/s | 0.984× | 1673.0 MiB/s | 1666.2 MiB/s | 1.004× |
| samba | 21606400 | 4.724 | 165.2 MiB/s | 171.1 MiB/s | 0.966× | 1659.2 MiB/s | 2057.5 MiB/s | 0.806× |
| sao | 7251944 | 1.375 | 96.1 MiB/s | 89.9 MiB/s | 1.069× | 1215.5 MiB/s | 1309.7 MiB/s | 0.928× |
| webster | 41458703 | 3.836 | 112.7 MiB/s | 111.3 MiB/s | 1.013× | 904.4 MiB/s | 913.2 MiB/s | 0.990× |
| x-ray | 8474240 | 1.526 | 108.8 MiB/s | 104.6 MiB/s | 1.040× | 1023.3 MiB/s | 1078.3 MiB/s | 0.949× |
| xml | 5345280 | 9.777 | 224.7 MiB/s | 237.6 MiB/s | 0.946× | 3118.9 MiB/s | 2734.7 MiB/s | 1.140× |

## lz4

![lz4](charts/lz4.svg)

| File | Bytes | Ratio | Rust compress | C compress | / C | Rust decompress | C decompress | / C |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| dickens | 10192446 | 2.284 | 46.4 MiB/s | 47.1 MiB/s | 0.985× | 3236.1 MiB/s | 3744.1 MiB/s | 0.864× |
| mozilla | 51220480 | 2.309 | 84.2 MiB/s | 89.0 MiB/s | 0.946× | 1605.9 MiB/s | 1638.2 MiB/s | 0.980× |
| mr | 9970564 | 2.308 | 54.9 MiB/s | 55.9 MiB/s | 0.982× | 3616.2 MiB/s | 4218.1 MiB/s | 0.857× |
| nci | 33553445 | 8.889 | 107.2 MiB/s | 129.3 MiB/s | 0.829× | 5025.3 MiB/s | 5543.6 MiB/s | 0.906× |
| ooffice | 6152192 | 1.733 | 55.3 MiB/s | 58.7 MiB/s | 0.942× | 3036.0 MiB/s | 3366.4 MiB/s | 0.902× |
| osdb | 10085684 | 2.534 | 83.4 MiB/s | 87.7 MiB/s | 0.951× | 4149.5 MiB/s | 4676.4 MiB/s | 0.887× |
| reymont | 6627202 | 3.055 | 46.8 MiB/s | 47.7 MiB/s | 0.980× | 3483.3 MiB/s | 3676.1 MiB/s | 0.948× |
| samba | 21606400 | 3.508 | 99.8 MiB/s | 103.0 MiB/s | 0.969× | 3940.9 MiB/s | 4194.2 MiB/s | 0.940× |
| sao | 7251944 | 1.261 | 42.9 MiB/s | 44.2 MiB/s | 0.971× | 2861.3 MiB/s | 3089.4 MiB/s | 0.926× |
| webster | 41458703 | 2.945 | 62.5 MiB/s | 65.3 MiB/s | 0.957× | 1375.2 MiB/s | 1488.8 MiB/s | 0.924× |
| x-ray | 8474240 | 1.181 | 56.4 MiB/s | 62.2 MiB/s | 0.906× | 2941.0 MiB/s | 3166.3 MiB/s | 0.929× |
| xml | 5345280 | 6.844 | 130.5 MiB/s | 137.3 MiB/s | 0.951× | 5572.3 MiB/s | 6114.8 MiB/s | 0.911× |

## lz

![lz](charts/lz.svg)

| File | Bytes | Ratio | Rust compress | C compress | / C | Rust decompress | C decompress | / C |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| dickens | 10192446 | 2.935 | 53.3 MiB/s | 60.0 MiB/s | 0.888× | 616.8 MiB/s | 416.2 MiB/s | 1.482× |
| mozilla | 51220480 | 2.713 | 93.3 MiB/s | 114.1 MiB/s | 0.818× | 984.3 MiB/s | 588.9 MiB/s | 1.671× |
| mr | 9970564 | 2.954 | 83.2 MiB/s | 99.9 MiB/s | 0.832× | 866.1 MiB/s | 500.2 MiB/s | 1.731× |
| nci | 33553445 | 12.380 | 182.1 MiB/s | 188.3 MiB/s | 0.967× | 1477.7 MiB/s | 1320.2 MiB/s | 1.119× |
| ooffice | 6152192 | 1.972 | 74.8 MiB/s | 91.1 MiB/s | 0.821× | 702.8 MiB/s | 565.5 MiB/s | 1.243× |
| osdb | 10085684 | 2.945 | 107.2 MiB/s | 128.2 MiB/s | 0.836× | 1082.6 MiB/s | 786.1 MiB/s | 1.377× |
| reymont | 6627202 | 3.651 | 64.8 MiB/s | 83.5 MiB/s | 0.776× | 1052.7 MiB/s | 562.5 MiB/s | 1.871× |
| samba | 21606400 | 4.437 | 106.9 MiB/s | 137.0 MiB/s | 0.780× | 1265.6 MiB/s | 898.4 MiB/s | 1.409× |
| sao | 7251944 | 1.370 | 72.6 MiB/s | 90.1 MiB/s | 0.806× | 787.1 MiB/s | 589.0 MiB/s | 1.336× |
| webster | 41458703 | 3.739 | 75.2 MiB/s | 87.3 MiB/s | 0.861× | 887.2 MiB/s | 554.9 MiB/s | 1.599× |
| x-ray | 8474240 | 1.477 | 83.4 MiB/s | 96.7 MiB/s | 0.862× | 734.3 MiB/s | 401.7 MiB/s | 1.828× |
| xml | 5345280 | 8.950 | 154.6 MiB/s | 168.7 MiB/s | 0.917× | 1927.5 MiB/s | 1331.5 MiB/s | 1.448× |

## huffman

![huffman](charts/huffman.svg)

| File | Bytes | Ratio | Rust compress | C compress | / C | Rust decompress | C decompress | / C |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| dickens | 10192446 | 1.748 | 1112.0 MiB/s | 1171.5 MiB/s | 0.949× | 1413.7 MiB/s | 2020.4 MiB/s | 0.700× |
| mozilla | 51220480 | 1.441 | 932.0 MiB/s | 953.2 MiB/s | 0.978× | 1000.3 MiB/s | 1276.3 MiB/s | 0.784× |
| mr | 9970564 | 2.250 | 998.6 MiB/s | 1051.9 MiB/s | 0.949× | 1333.2 MiB/s | 1892.2 MiB/s | 0.705× |
| nci | 33553445 | 3.256 | 1084.6 MiB/s | 1157.1 MiB/s | 0.937× | 1374.4 MiB/s | 1755.5 MiB/s | 0.783× |
| ooffice | 6152192 | 1.268 | 1044.3 MiB/s | 1102.6 MiB/s | 0.947× | 992.7 MiB/s | 1510.9 MiB/s | 0.657× |
| osdb | 10085684 | 1.202 | 980.6 MiB/s | 1048.9 MiB/s | 0.935× | 1122.2 MiB/s | 1379.6 MiB/s | 0.813× |
| reymont | 6627202 | 1.658 | 951.3 MiB/s | 1314.5 MiB/s | 0.724× | 1535.2 MiB/s | 1927.7 MiB/s | 0.796× |
| samba | 21606400 | 1.514 | 1034.1 MiB/s | 1155.7 MiB/s | 0.895× | 1117.9 MiB/s | 1510.4 MiB/s | 0.740× |
| sao | 7251944 | 1.074 | 1050.9 MiB/s | 1139.7 MiB/s | 0.922× | 1050.2 MiB/s | 1458.3 MiB/s | 0.720× |
| webster | 41458703 | 1.599 | 996.5 MiB/s | 1051.1 MiB/s | 0.948× | 1279.5 MiB/s | 1546.5 MiB/s | 0.827× |
| x-ray | 8474240 | 1.247 | 930.7 MiB/s | 1078.1 MiB/s | 0.863× | 1047.0 MiB/s | 1309.6 MiB/s | 0.799× |
| xml | 5345280 | 1.609 | 1151.9 MiB/s | 1304.9 MiB/s | 0.883× | 1535.4 MiB/s | 1967.2 MiB/s | 0.781× |

## fse

![fse](charts/fse.svg)

| File | Bytes | Ratio | Rust compress | C compress | / C | Rust decompress | C decompress | / C |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| dickens | 10192446 | 1.761 | 535.5 MiB/s | 538.0 MiB/s | 0.995× | 823.9 MiB/s | 927.1 MiB/s | 0.889× |
| mozilla | 51220480 | 1.443 | 465.6 MiB/s | 507.2 MiB/s | 0.918× | 920.9 MiB/s | 1026.8 MiB/s | 0.897× |
| mr | 9970564 | 2.272 | 553.5 MiB/s | 625.2 MiB/s | 0.885× | 855.1 MiB/s | 1039.3 MiB/s | 0.823× |
| nci | 33553445 | 3.273 | 622.8 MiB/s | 649.2 MiB/s | 0.959× | 805.1 MiB/s | 981.3 MiB/s | 0.820× |
| ooffice | 6152192 | 1.266 | 621.4 MiB/s | 654.0 MiB/s | 0.950× | 1021.0 MiB/s | 1118.1 MiB/s | 0.913× |
| osdb | 10085684 | 1.202 | 612.3 MiB/s | 641.8 MiB/s | 0.954× | 960.6 MiB/s | 1088.2 MiB/s | 0.883× |
| reymont | 6627202 | 1.665 | 665.8 MiB/s | 696.5 MiB/s | 0.956× | 1052.7 MiB/s | 1196.0 MiB/s | 0.880× |
| samba | 21606400 | 1.520 | 623.4 MiB/s | 554.8 MiB/s | 1.124× | 951.7 MiB/s | 1059.5 MiB/s | 0.898× |
| sao | 7251944 | 1.070 | 612.9 MiB/s | 655.2 MiB/s | 0.935× | 1049.8 MiB/s | 1196.0 MiB/s | 0.878× |
| webster | 41458703 | 1.607 | 628.3 MiB/s | 634.2 MiB/s | 0.991× | 921.2 MiB/s | 1015.9 MiB/s | 0.907× |
| x-ray | 8474240 | 1.245 | 585.4 MiB/s | 639.6 MiB/s | 0.915× | 972.5 MiB/s | 1167.1 MiB/s | 0.833× |
| xml | 5345280 | 1.618 | 652.1 MiB/s | 636.1 MiB/s | 1.025× | 1021.8 MiB/s | 1205.7 MiB/s | 0.847× |

## entropy

![entropy](charts/entropy.svg)

| File | Bytes | Ratio | Rust compress | C compress | / C | Rust decompress | C decompress | / C |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| dickens | 10192446 | 1.748 | 1098.8 MiB/s | 1117.8 MiB/s | 0.983× | 1418.0 MiB/s | 1862.3 MiB/s | 0.761× |
| mozilla | 51220480 | 1.442 | 933.2 MiB/s | 989.7 MiB/s | 0.943× | 1007.4 MiB/s | 1298.5 MiB/s | 0.776× |
| mr | 9970564 | 2.258 | 929.6 MiB/s | 1015.7 MiB/s | 0.915× | 1314.0 MiB/s | 1718.2 MiB/s | 0.765× |
| nci | 33553445 | 3.256 | 929.6 MiB/s | 1083.9 MiB/s | 0.858× | 1412.3 MiB/s | 1430.2 MiB/s | 0.987× |
| ooffice | 6152192 | 1.268 | 999.1 MiB/s | 1027.8 MiB/s | 0.972× | 1071.0 MiB/s | 1366.2 MiB/s | 0.784× |
| osdb | 10085684 | 1.202 | 907.8 MiB/s | 1052.7 MiB/s | 0.862× | 935.5 MiB/s | 1311.6 MiB/s | 0.713× |
| reymont | 6627202 | 1.658 | 993.0 MiB/s | 1268.4 MiB/s | 0.783× | 1534.4 MiB/s | 1898.8 MiB/s | 0.808× |
| samba | 21606400 | 1.514 | 914.2 MiB/s | 1042.9 MiB/s | 0.877× | 972.7 MiB/s | 1374.1 MiB/s | 0.708× |
| sao | 7251944 | 1.074 | 864.5 MiB/s | 1070.6 MiB/s | 0.808× | 1062.4 MiB/s | 1244.1 MiB/s | 0.854× |
| webster | 41458703 | 1.599 | 976.2 MiB/s | 937.4 MiB/s | 1.041× | 1194.5 MiB/s | 1547.1 MiB/s | 0.772× |
| x-ray | 8474240 | 1.247 | 920.7 MiB/s | 1005.3 MiB/s | 0.916× | 1037.9 MiB/s | 1367.9 MiB/s | 0.759× |
| xml | 5345280 | 1.609 | 885.9 MiB/s | 1310.3 MiB/s | 0.676× | 1572.6 MiB/s | 1752.1 MiB/s | 0.898× |

## huffman_pivco

![huffman_pivco](charts/huffman_pivco.svg)

| File | Bytes | Ratio | Rust compress | C compress | / C | Rust decompress | C decompress | / C |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| dickens | 10192446 | 1.745 | 1086.8 MiB/s | 961.2 MiB/s | 1.131× | 3129.0 MiB/s | 3061.7 MiB/s | 1.022× |
| mozilla | 51220480 | 1.439 | 870.5 MiB/s | 860.9 MiB/s | 1.011× | 1871.0 MiB/s | 1772.7 MiB/s | 1.055× |
| mr | 9970564 | 2.247 | 1221.8 MiB/s | 941.0 MiB/s | 1.298× | 3328.0 MiB/s | 3302.0 MiB/s | 1.008× |
| nci | 33553445 | 3.251 | 1502.5 MiB/s | 1078.1 MiB/s | 1.394× | 2841.2 MiB/s | 2658.2 MiB/s | 1.069× |
| ooffice | 6152192 | 1.266 | 946.1 MiB/s | 928.7 MiB/s | 1.019× | 2923.8 MiB/s | 2737.9 MiB/s | 1.068× |
| osdb | 10085684 | 1.200 | 793.7 MiB/s | 870.3 MiB/s | 0.912× | 2333.7 MiB/s | 2343.5 MiB/s | 0.996× |
| reymont | 6627202 | 1.656 | 1124.1 MiB/s | 1083.3 MiB/s | 1.038× | 3973.8 MiB/s | 3305.2 MiB/s | 1.202× |
| samba | 21606400 | 1.512 | 957.9 MiB/s | 888.7 MiB/s | 1.078× | 2078.5 MiB/s | 2049.3 MiB/s | 1.014× |
| sao | 7251944 | 1.073 | 846.6 MiB/s | 792.1 MiB/s | 1.069× | 2672.0 MiB/s | 2602.1 MiB/s | 1.027× |
| webster | 41458703 | 1.597 | 979.0 MiB/s | 893.9 MiB/s | 1.095× | 2001.7 MiB/s | 1950.8 MiB/s | 1.026× |
| x-ray | 8474240 | 1.246 | 945.3 MiB/s | 927.7 MiB/s | 1.019× | 2783.1 MiB/s | 2864.5 MiB/s | 0.972× |
| xml | 5345280 | 1.607 | 1157.0 MiB/s | 1147.2 MiB/s | 1.009× | 3939.6 MiB/s | 3317.1 MiB/s | 1.188× |

## flatpack

![flatpack](charts/flatpack.svg)

| File | Bytes | Ratio | Rust compress | C compress | / C | Rust decompress | C decompress | / C |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| dickens | 10192446 | 1.143 | 1867.6 MiB/s | 1100.3 MiB/s | 1.697× | 2442.9 MiB/s | 1444.5 MiB/s | 1.691× |
| mozilla | 51220480 | 1.000 | 1067.5 MiB/s | 807.5 MiB/s | 1.322× | 7645.0 MiB/s | 5506.9 MiB/s | 1.388× |
| mr | 9970564 | 1.000 | 1863.3 MiB/s | 1332.4 MiB/s | 1.398× | 16769.2 MiB/s | 11526.9 MiB/s | 1.455× |
| nci | 33553445 | 1.333 | 1709.4 MiB/s | 1024.1 MiB/s | 1.669× | 2144.5 MiB/s | 1465.1 MiB/s | 1.464× |
| ooffice | 6152192 | 1.000 | 2010.6 MiB/s | 1350.8 MiB/s | 1.488× | 18712.5 MiB/s | 12523.0 MiB/s | 1.494× |
| osdb | 10085684 | 1.000 | 1784.6 MiB/s | 1294.5 MiB/s | 1.379× | 16892.9 MiB/s | 10630.3 MiB/s | 1.589× |
| reymont | 6627202 | 1.000 | 1980.2 MiB/s | 1326.6 MiB/s | 1.493× | 18685.5 MiB/s | 12438.1 MiB/s | 1.502× |
| samba | 21606400 | 1.000 | 1355.0 MiB/s | 1042.4 MiB/s | 1.300× | 10164.6 MiB/s | 6648.6 MiB/s | 1.529× |
| sao | 7251944 | 1.000 | 1876.4 MiB/s | 1289.5 MiB/s | 1.455× | 17993.9 MiB/s | 12109.4 MiB/s | 1.486× |
| webster | 41458703 | 1.143 | 1080.6 MiB/s | 716.5 MiB/s | 1.508× | 1971.4 MiB/s | 1336.0 MiB/s | 1.476× |
| x-ray | 8474240 | 1.000 | 1831.7 MiB/s | 1303.5 MiB/s | 1.405× | 17436.2 MiB/s | 11810.6 MiB/s | 1.476× |
| xml | 5345280 | 1.143 | 2042.6 MiB/s | 1174.4 MiB/s | 1.739× | 2511.3 MiB/s | 1480.2 MiB/s | 1.697× |

## generic

![generic](charts/generic.svg)

| File | Bytes | Ratio | Rust compress | C compress | / C | Rust decompress | C decompress | / C |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| dickens | 10192446 | 2.982 | 95.7 MiB/s | 97.0 MiB/s | 0.987× | 1135.7 MiB/s | 1365.1 MiB/s | 0.832× |
| mozilla | 51220480 | 2.986 | 137.1 MiB/s | 140.3 MiB/s | 0.978× | 1198.7 MiB/s | 1242.6 MiB/s | 0.965× |
| mr | 9970564 | 2.944 | 124.0 MiB/s | 127.4 MiB/s | 0.973× | 1413.4 MiB/s | 1474.7 MiB/s | 0.958× |
| nci | 33553445 | 13.830 | 246.9 MiB/s | 250.5 MiB/s | 0.986× | 2477.2 MiB/s | 2788.2 MiB/s | 0.888× |
| ooffice | 6152192 | 2.099 | 103.8 MiB/s | 109.2 MiB/s | 0.951× | 1016.4 MiB/s | 1240.4 MiB/s | 0.819× |
| osdb | 10085684 | 2.970 | 138.4 MiB/s | 141.9 MiB/s | 0.976× | 1881.5 MiB/s | 1770.8 MiB/s | 1.062× |
| reymont | 6627202 | 3.735 | 99.6 MiB/s | 98.3 MiB/s | 1.013× | 1538.2 MiB/s | 1616.3 MiB/s | 0.952× |
| samba | 21606400 | 4.724 | 163.6 MiB/s | 181.3 MiB/s | 0.902× | 1855.2 MiB/s | 1929.9 MiB/s | 0.961× |
| sao | 7251944 | 1.375 | 99.9 MiB/s | 103.3 MiB/s | 0.967× | 1202.4 MiB/s | 1190.8 MiB/s | 1.010× |
| webster | 41458703 | 3.836 | 112.9 MiB/s | 115.7 MiB/s | 0.976× | 1278.5 MiB/s | 1196.3 MiB/s | 1.069× |
| x-ray | 8474240 | 1.526 | 100.0 MiB/s | 102.1 MiB/s | 0.980× | 882.5 MiB/s | 1021.2 MiB/s | 0.864× |
| xml | 5345280 | 9.777 | 226.1 MiB/s | 236.3 MiB/s | 0.957× | 3063.2 MiB/s | 3163.0 MiB/s | 0.968× |

## field_lz_struct4

![field_lz_struct4](charts/field_lz_struct4.svg)

| File | Bytes | Ratio | Rust compress | C compress | / C | Rust decompress | C decompress | / C |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| sao | 7251944 | 1.471 | 41.1 MiB/s | 59.9 MiB/s | 0.686× | 1064.6 MiB/s | 1314.9 MiB/s | 0.810× |
| osdb | 10085684 | 3.027 | 47.9 MiB/s | 58.0 MiB/s | 0.827× | 1256.5 MiB/s | 1646.8 MiB/s | 0.763× |
| nci | 33553444 | 12.506 | 98.2 MiB/s | 111.3 MiB/s | 0.882× | 1438.2 MiB/s | 2126.4 MiB/s | 0.676× |

## bitpack_num16

![bitpack_num16](charts/bitpack_num16.svg)

| File | Bytes | Ratio | Rust compress | C compress | / C | Rust decompress | C decompress | / C |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| mr | 9970564 | 1.000 | 7730.2 MiB/s | 4116.1 MiB/s | 1.878× | 15751.9 MiB/s | 7147.8 MiB/s | 2.204× |
| x-ray | 8474240 | 1.000 | 8454.3 MiB/s | 5061.8 MiB/s | 1.670× | 17223.1 MiB/s | 7511.5 MiB/s | 2.293× |

## numeric16

![numeric16](charts/numeric16.svg)

| File | Bytes | Ratio | Rust compress | C compress | / C | Rust decompress | C decompress | / C |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| mr | 9970564 | 3.178 | 30.7 MiB/s | 33.3 MiB/s | 0.923× | 1046.0 MiB/s | 1276.0 MiB/s | 0.820× |
| x-ray | 8474240 | 1.636 | 21.2 MiB/s | 22.0 MiB/s | 0.963× | 590.4 MiB/s | 759.6 MiB/s | 0.777× |

## numeric32

![numeric32](charts/numeric32.svg)

| File | Bytes | Ratio | Rust compress | C compress | / C | Rust decompress | C decompress | / C |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| sao | 7251944 | 1.471 | 43.8 MiB/s | 63.0 MiB/s | 0.694× | 1053.5 MiB/s | 1276.9 MiB/s | 0.825× |
| osdb | 10085684 | 3.027 | 45.5 MiB/s | 55.0 MiB/s | 0.827× | 1077.0 MiB/s | 1525.0 MiB/s | 0.706× |
| nci | 33553444 | 12.506 | 106.3 MiB/s | 111.0 MiB/s | 0.958× | 1254.5 MiB/s | 2009.0 MiB/s | 0.624× |

## generic_num16

![generic_num16](charts/generic_num16.svg)

| File | Bytes | Ratio | Rust compress | C compress | / C | Rust decompress | C decompress | / C |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| mr | 9970564 | 3.178 | 28.8 MiB/s | 35.2 MiB/s | 0.819× | 1020.1 MiB/s | 1263.7 MiB/s | 0.807× |
| x-ray | 8474240 | 1.636 | 20.2 MiB/s | 21.5 MiB/s | 0.936× | 587.6 MiB/s | 696.1 MiB/s | 0.844× |

## generic_num32

![generic_num32](charts/generic_num32.svg)

| File | Bytes | Ratio | Rust compress | C compress | / C | Rust decompress | C decompress | / C |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| sao | 7251944 | 1.471 | 42.5 MiB/s | 57.7 MiB/s | 0.737× | 993.8 MiB/s | 1163.8 MiB/s | 0.854× |
| osdb | 10085684 | 3.027 | 42.0 MiB/s | 49.8 MiB/s | 0.844× | 1019.5 MiB/s | 1436.8 MiB/s | 0.710× |
| nci | 33553444 | 12.506 | 99.9 MiB/s | 101.2 MiB/s | 0.987× | 1210.8 MiB/s | 2066.9 MiB/s | 0.586× |

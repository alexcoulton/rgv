cd /Users/alexcoulton/work/external.repos/rgv/rgv-main

cargo run --release -p rgv-desktop -- \
    --bam benchmark-data/large-public-alignments/ENCFF686MTA.bam \
    --reference benchmark-data/large-public-alignments/reference/GRCh38_V29_UCSC_names_ERCC_phiX.fa \
    --locus chr7:5526400-5530600 \
    --max-alignments 10000000

cargo run --release -p rgv-desktop -- \
    --reference benchmark-data/large-public-alignments/reference/GRCh38_V29_UCSC_names_ERCC_phiX.fa

############################
#RUN WITH TRACE 
############################

cargo run --release -p rgv-desktop -- \
    --reference benchmark-data/large-public-alignments/reference/GRCh38_V29_UCSC_names_ERCC_phiX.fa \
    --start-diagnostic-trace \
    --diagnostic-trace-dir /Users/alexcoulton/Desktop


cargo run --release -p rgv-desktop -- \
    --reference benchmark-data/large-public-alignments/reference/human_g1k_v37_decoy.fasta \
    --start-diagnostic-trace \
    --diagnostic-trace-dir /Users/alexcoulton/Desktop




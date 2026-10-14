# Mukon default baseline

This branch starts at upstream `aeyakovenko/percolator-prog` commit
`5cb331dde354517c6371a8acf92cecb194f3bb73`.

The previous Mukon default commit `36fa587e1424a8c7fdde2c701c10938188a51876`
contains no Mukon-only source changes and is preserved at
`archive/default-main-36fa587-20261013`.

The deployed Ninja product is intentionally not merged into this default line.
Its reviewed source lineage is preserved separately at
`archive/product-6e0ca331-20261013`, and the local fee-clamp integration is on
`feat/normalize-product-20261013`. The product line forked from upstream
`186e9c6e189735ec43923dac7fcad1f8991a7ddd`; later upstream changes include
account-layout and security evolution that require an explicit migration rather
than a merge into deployed accounts.

The default baseline is therefore suitable for upstream comparison and future
clean-room development, not as a deployment artifact for program
`7C37Xn3NLknqmSaxASYy2uRkb1RQcXigPmJCANUNYnvq`.

Moving a remote default branch to this line requires separate approval and a
backup/force-with-lease plan. This file authorizes no push, program deployment,
transaction, signer use, or authority change.

# Pozzo: A Fast Lucky Number Checker
[*"Think! Stop! Forward! Think!"*](https://www.sensortime.com/think_pig.html)

Pozzo tests large integers for luckiness. It dramatically more efficient than its predecessors, and increases the number of values searched by a factor between $10^3$ and $10^8$ for the below sequences.

Bold values are newly discovered.


## Sequences
### Lucky Repdigits

1, 3, 7, 9, 33, 99, 111, 777, 9999, 33333, 55555, 111111,
777777, 7777777, 55555555, **777777777777**, **9999999999999**.

Lucky decimal repdigits. ([A031882](https://oeis.org/A031882))

Old bound: `a(16) > 10^9`

New bound: `a(18) >= 777777777777777`

Factor more searched: $8 * 10^5$

### Lucky Mersennes

1, 3, 7, 15, 31, 63, 127, 511, 1023, 4095, 8191, 131071,
524287, 2097151, 4194303, 8388607, 33554431, 67108863, 8589934591,
**68719476735**, **1099511627775**, **4398046511103**.

Lucky numbers of the form `2^k - 1`. ([A057613](https://oeis.org/A057613))

Old bound: `a(20) >= 17179869183`

New bound: `a(23) >= 562949953421311`

Factor more searched: $3 * 10^4$

### Lucky Mersennes With Prime Exponent

3, 7, 31, 127, 8191, 131071, 524287, 8388607.

Lucky numbers of the form `2^p - 1` for prime `p`. ([A057612](https://oeis.org/A057612))

Old bound: `a(9) > 2^31 - 1`

New bound: `a(9) >= 576460752303423487`

Factor more searched: $3 * 10^8$

### Lucky Fibonacci Numbers

1, 3, 13, 21, 1597, 6765, 75025, **32951280099**.

Lucky Fibonacci numbers. ([A057589](https://oeis.org/A057589))

Old bound: `a(8) >= 12586269025`

New bound: `a(9) >= 72723460248141`

Factor more searched: $6 * 10^3$

### Lucky Lucas Numbers

1, 3, 7, 3571, 9349, 710647, 12752043.

Lucky Lucas numbers. ([A306632](https://oeis.org/A306632))

Old bound: `a(8) >= 10^9`

New bound: `a(8) >= 100501350283429`

Factor more searched: $1 * 10^5$

### Lucky Tetranacci Numbers

1, 15, 10671, **274423830033**.

Lucky tetranacci numbers. ([A140285](https://oeis.org/A140285))

Old bound: `a(4) >= 10312882481`

New bound: `a(5) >= 194314552299285`

Factor more searched: $2 * 10^4$

### Lucky Consecutive-Digit Numbers

21, 43, 67, 87, 321, 4321, 4567, 6789, 78901, 432109, 9012345,
67890123, 109876543, 123456789, 6543210987, 8901234567,
**9876543210987**.

Lucky numbers with ascending or descending cyclic consecutive decimal digits. ([A118569](https://oeis.org/A118569))

Old bound: `a(17) >= 10^10`

New bound: `a(18) >= 123456789012345`

Factor more searched: $1 * 10^4$

### Lucky Numbers

Lucky numbers are produced by a sieve. From the [OEIS Wiki](https://oeis.org/wiki/Lucky_numbers):

Start with a sequence of positive odd numbers:

```text
1, 3, 5, 7, 9, 11, 13, 15, 17, 19, 21, ...
```

The first nonunit survivor is `3`, so strike every third value in this list:

```text
1, 3, [5], 7, 9, [11], 13, 15, [17], 19, 21, [23], 25, 27, [29], 31, ...
```

The next unused survivor is `7`, so strike every seventh value of the new list.
```text
1, 3, 7, 9, 13, 15, [19], 21, 25, 27, 31, 33, 37, [39], 43, ...
```

The values that are never struck out are the lucky numbers:

```text
1, 3, 7, 9, 13, 15, 21, 25, 31, ...
```

They are interesting because they share many prime-like statistical properties, even though membership is defined by survival in this position sieve rather than by factors.

## Main Idea

To check `k` numbers, we maintain a fenwick tree over `k` bits, where each node counts the number of set bits in its range. By traversing this tree, we can very quickly look up and unset the `i`th set bit.

Memory constrains the size of the sieve. The sieve requires two bits per odd integer (one bit for the bitset, and one amortized bit for the fenwick tree). This averages out to one integer per bit of RAM.

Note that this does not bound the maximum size of the numbers we can prove lucky; we can prove luckiness of candidates much higher than the sieve limit, with the following algorithm:

1. Start each candidate at `rank = (n + 1) / 2`, the rank among odd numbers after deleting evens.
2. For each lucky deletion factor `l`, reject when `rank % l == 0`.
3. Otherwise update `rank -= rank / l`.
4. Once `rank < l`, the candidate has survived all future deletions and is lucky.

## Run
The reported run took about 12 hours on a machine with 128GB of RAM. The sieve covers approximately $2^{40}$ integers. The log can be found in `pozzo.log`.

## Usage

```sh
cargo test
cargo run --release -- --memory-mib 114688
```

## Motivation
> Hello! And what hackathon project are you presenting today, young man? 

> The integer 4,398,046,511,103.

After several hackathons spent making things like "Uber for dogs with hearing loss", I decided that my next project would be the integer 4,398,046,511,103.

It was a very [Duchamp](https://en.wikipedia.org/wiki/Fountain_(Duchamp)) era of my life.

I started this in 2019 and would certainly not have finished it without 2026-era coding agents, to whom most credit is due.
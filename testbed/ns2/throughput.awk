$1 == "r" && $4 == 3 { bin = int($2 / 0.5); bytes[$8, bin] += $6; if (bin > last) last = bin }
END { for (b = 0; b <= last; b++) printf "%.1f %.3f %.3f\n", b * 0.5, bytes[1, b] * 8 / 0.5e6, bytes[2, b] * 8 / 0.5e6 }

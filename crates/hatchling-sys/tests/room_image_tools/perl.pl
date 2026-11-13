open(my $f, "<", "numbers.txt") or die;
my @lines = <$f>;
print scalar(@lines), "\n";
system("true") == 0 or die;
mkdir "pl" or die;
rename "pl", "pl2" or die;

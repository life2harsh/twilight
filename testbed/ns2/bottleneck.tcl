set loss [lindex $argv 0]
set out [lindex $argv 1]
set ns [new Simulator]
set tr [open $out w]
$ns trace-all $tr
set h1 [$ns node]; set h2 [$ns node]; set r [$ns node]; set c [$ns node]
$ns duplex-link $h1 $r 100Mb 1ms DropTail
$ns duplex-link $h2 $r 100Mb 1ms DropTail
$ns duplex-link $r $c 10Mb 10ms DropTail
$ns queue-limit $r $c 50
if {$loss > 0} {
  set em [new ErrorModel]
  $em set rate_ $loss
  $em unit pkt
  $em ranvar [new RandomVariable/Uniform]
  $em drop-target [new Agent/Null]
  $ns lossmodel $em $r $c
}
set tcp [new Agent/TCP/Reno]; $tcp set fid_ 1; $tcp set packetSize_ 1200; $tcp set window_ 200
$ns attach-agent $h1 $tcp
set sink [new Agent/TCPSink]; $ns attach-agent $c $sink; $ns connect $tcp $sink
set ftp [new Application/FTP]; $ftp attach-agent $tcp
set udp [new Agent/UDP]; $udp set fid_ 2; $ns attach-agent $h2 $udp
set null [new Agent/Null]; $ns attach-agent $c $null; $ns connect $udp $null
set cbr [new Application/Traffic/CBR]; $cbr attach-agent $udp
$cbr set packetSize_ 1200; $cbr set rate_ 6Mb
$ns at 0.0 "$ftp start"
$ns at 10.0 "$cbr start"
$ns at 20.0 "$cbr stop"
$ns at 30.0 "close $tr; exit 0"
$ns run

#!/bin/sh
# Use Ubuntu's signed primary archive on runners with an EC2 mirror list.
set -eu
sudo sed -Ei '/^URIs:/s#(mirror\+file:/etc/apt/apt-mirrors\.txt|https?://[[:alnum:]-]+\.ec2\.archive\.ubuntu\.com/ubuntu/?)([[:space:]]|$)#https://archive.ubuntu.com/ubuntu/\2#' /etc/apt/sources.list.d/ubuntu.sources

#!/bin/bash

query="$1"
dict_id="$2"

source ./lib.sh
search_dict "$dict_id" "$query"

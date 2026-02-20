#!/usr/bin/env bash

CURRENT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

get_tmux_option() {
	local option="$1"
	local default_value="$2"
	local option_value
	option_value=$(tmux show-option -gqv "$option" 2>/dev/null || echo "")
	if [[ -z "$option_value" ]]; then
		echo "$default_value"
	else
		echo "$option_value"
	fi
}

set_default_options() {
	local path_option
	path_option=$(get_tmux_option "@mxsh-path" "")
	if [[ -z "$path_option" ]]; then
		tmux set-option -g @mxsh-path "$HOME/lab"
	fi

	local key_option
	key_option=$(get_tmux_option "@mxsh-key" "")
	if [[ -z "$key_option" ]]; then
		tmux set-option -g @mxsh-key "s"
	fi

	local height_option
	height_option=$(get_tmux_option "@mxsh-height" "")
	if [[ -z "$height_option" ]]; then
		tmux set-option -g @mxsh-height "40"
	fi

	local width_option
	width_option=$(get_tmux_option "@mxsh-width" "")
	if [[ -z "$width_option" ]]; then
		tmux set-option -g @mxsh-width "60"
	fi
}

setup_key_binding() {
	local key
	key=$(get_tmux_option "@mxsh-key" "s")
	local height
	height=$(get_tmux_option "@mxsh-height" "40")
	local width
	width=$(get_tmux_option "@mxsh-width" "60")

	tmux bind "$key" display-popup -w "${width}%" -h "${height}%" -E "$CURRENT_DIR/bin/picker"
}

main() {
	set_default_options
	setup_key_binding
}

main "$@"

use std::env;
use std::process;

fn print_usage(program_name: &str) {
    println!("Usage:");
    println!("  {program_name} [options]");
    println!();
    println!("Options:");
    println!("  -p, --pretty   show downtime in pretty format");
    println!("  -h, --help     display this help and exit");
    println!("  -s, --since    system down since");
    println!("  -V, --version  output version information and exit");
}

fn main() {
    let mut help = false;
    let mut pretty = false;
    let mut since = false;
    let mut version = false;

    let args: Vec<String> = env::args().collect();
    let program_name = &args[0];
    for arg in args.iter().skip(1) {
        if arg.starts_with("--") {
            match arg.as_str() {
                "--help" => help = true,
                "--pretty" => pretty = true,
                "--since" => since = true,
                "--version" => version = true,
                _ => {
                    println!("Unrecognized option `{arg}`");
                    print_usage(program_name);
                    process::exit(1);
                }
            }
        } else if arg.starts_with("-") {
            for char in arg[1..].chars() {
                match char {
                    'h' => help = true,
                    'p' => pretty = true,
                    's' => since = true,
                    'V' => version = true,
                    _ => {
                        println!("Unrecognized option `-{char}`");
                        print_usage(program_name);
                        process::exit(1);
                    }
                }
            }
        } else {
            println!("Unrecognized argument `{}`", arg);
            print_usage(program_name);
            process::exit(1);
        }
    }

    if help {
        print_usage(program_name);
        return;
    } else if version {
        println!("downtime version 1.0");
        return;
    } else if since {
        println!("System is not down.");
        return;
    } else if pretty {
        println!("System is not down.");
        return;
    }

    println!("System is not down.");
}

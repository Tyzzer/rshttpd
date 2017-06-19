error_chain!{
    foreign_links {
        Io(::std::io::Error);
        Nix(::nix::Error);
        SendError(::futures::sync::mpsc::SendError<::hyper::Result<::hyper::Chunk>>);
    }
}

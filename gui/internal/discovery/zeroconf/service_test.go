package zeroconf

import (
	"context"
	"log"
	"testing"
	"time"

	"github.com/pkg/errors"
)

var (
	mdnsName    = "test--xxxxxxxxxxxx"
	mdnsService = "test--xxxx.tcp"
	mdnsDomain  = "local."
	mdnsPort    = 8888
)

func startMDNS(ctx context.Context, port int, name, service, domain string) {
	// 5353 is default mdns port
	server, err := Register(name, service, domain, port, []string{"txtv=0", "lo=1", "la=2"}, nil)
	if err != nil {
		panic(errors.Wrap(err, "while registering mdns service"))
	}
	defer server.Shutdown()
	log.Printf("Published service: %s, type: %s, domain: %s", name, service, domain)

	<-ctx.Done()

	log.Printf("Shutting down.")

}

func TestBasic(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	go startMDNS(ctx, mdnsPort, mdnsName, mdnsService, mdnsDomain)

	time.Sleep(time.Second)

	resolver, err := NewResolver(nil)
	if err != nil {
		t.Fatalf("Expected create resolver success, but got %v", err)
	}
	entries := make(chan *ServiceEntry, 16)

	if err := resolver.Browse(ctx, mdnsService, mdnsDomain, entries); err != nil {
		t.Fatalf("Expected browse success, but got %v", err)
	}
	<-ctx.Done()

	// VENDORED-FIX: upstream appended to a plain slice from a goroutine
	// and asserted on it from the test goroutine — a data race. The
	// channel is buffered, so draining it here (after ctx.Done) is
	// race-free: everything sent before the browse ended is received.
	var found *ServiceEntry
	drained := false
	for !drained {
		select {
		case s := <-entries:
			if s != nil && s.Service == mdnsService {
				if found == nil {
					found = s
				}
			}
		default:
			drained = true
		}
	}

	if found == nil {
		t.Fatalf("Expected number of service entries is 1, but got 0")
	}
	if found.Domain != mdnsDomain {
		t.Fatalf("Expected domain is %s, but got %s", mdnsDomain, found.Domain)
	}
	if found.Service != mdnsService {
		t.Fatalf("Expected service is %s, but got %s", mdnsService, found.Service)
	}
	if found.Instance != mdnsName {
		t.Fatalf("Expected instance is %s, but got %s", mdnsName, found.Instance)
	}
	if found.Port != mdnsPort {
		t.Fatalf("Expected port is %d, but got %d", mdnsPort, found.Port)
	}
}

func TestNoRegister(t *testing.T) {
	resolver, err := NewResolver(nil)
	if err != nil {
		t.Fatalf("Expected create resolver success, but got %v", err)
	}

	// before register, mdns resolve shuold not have any entry
	entries := make(chan *ServiceEntry, 1)

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	if err := resolver.Browse(ctx, mdnsService, mdnsDomain, entries); err != nil {
		t.Fatalf("Expected browse success, but got %v", err)
	}
	<-ctx.Done()
	cancel()
	// VENDORED-FIX: upstream received in a goroutine and called
	// t.Fatalf there (illegal, and racy with the test goroutine); the
	// buffered channel makes a drain here equivalent and safe.
	select {
	case s := <-entries:
		if s != nil {
			t.Fatalf("Expected empty service entries but got %v", *s)
		}
	default:
	}
}
